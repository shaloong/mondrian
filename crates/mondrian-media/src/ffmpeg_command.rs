//! Spawn authority travels with a command and the native child it creates.

use std::ffi::OsStr;
use std::io;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// A media command whose qualified execution authority cannot be detached.
///
/// Only arguments and standard streams are mutable. The executable, current
/// directory and loader environment of an admitted capsule are private.
#[derive(Debug)]
pub struct FfmpegCommand {
    command: Command,
    #[cfg(feature = "validation")]
    authority: Option<std::sync::Arc<crate::PreparedFfmpegToolchain>>,
}

impl FfmpegCommand {
    /// Construct an ordinary command. Qualification callers use `ffmpeg_command`.
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self::ordinary(Command::new(program))
    }

    pub(crate) fn ordinary(command: Command) -> Self {
        Self {
            command,
            #[cfg(feature = "validation")]
            authority: None,
        }
    }

    #[cfg(feature = "validation")]
    pub(crate) fn qualified(
        command: Command,
        authority: std::sync::Arc<crate::PreparedFfmpegToolchain>,
    ) -> Self {
        Self { command, authority: Some(authority) }
    }

    /// Append an argument without granting access to native spawn.
    pub fn arg(&mut self, argument: impl AsRef<OsStr>) -> &mut Self {
        self.command.arg(argument);
        self
    }

    /// Append arguments without granting access to native spawn.
    pub fn args<I, S>(&mut self, arguments: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(arguments);
        self
    }

    /// Configure standard input.
    pub fn stdin(&mut self, stream: impl Into<Stdio>) -> &mut Self {
        self.command.stdin(stream);
        self
    }

    /// Configure standard output.
    pub fn stdout(&mut self, stream: impl Into<Stdio>) -> &mut Self {
        self.command.stdout(stream);
        self
    }

    /// Configure standard error.
    pub fn stderr(&mut self, stream: impl Into<Stdio>) -> &mut Self {
        self.command.stderr(stream);
        self
    }

    /// Inspect the immutable executable.
    pub fn get_program(&self) -> &OsStr {
        self.command.get_program()
    }

    /// Inspect the command arguments.
    pub fn get_args(&self) -> std::process::CommandArgs<'_> {
        self.command.get_args()
    }

    /// Inspect the immutable loader environment overrides.
    pub fn get_envs(&self) -> std::process::CommandEnvs<'_> {
        self.command.get_envs()
    }

    /// Inspect the immutable working directory.
    pub fn get_current_dir(&self) -> Option<&Path> {
        self.command.get_current_dir()
    }

    /// Set an ordinary helper environment value; qualified spawn rejects changes.
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.command.env(key, value);
        self
    }

    /// Set an ordinary helper directory; qualified spawn rejects changes.
    pub fn current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.command.current_dir(path);
        self
    }

    /// Hide a native helper window without changing its execution authority.
    pub fn hide_window(&mut self) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.command.creation_flags(0x0800_0000);
        }
    }

    /// Revalidate at spawn and retain the capsule until native exit is observed.
    pub fn spawn(&mut self) -> io::Result<FfmpegChild> {
        self.spawn_until(None)
    }

    fn spawn_until(&mut self, deadline: Option<Instant>) -> io::Result<FfmpegChild> {
        self.hide_window();
        #[cfg(feature = "validation")]
        let lease = match &self.authority {
            Some(authority) => Some(
                authority
                    .admit_child(&self.command)
                    .map_err(|error| io::Error::other(crate::FfmpegCommandError::from(error)))?,
            ),
            None => {
                crate::qualified_ffmpeg::reject_unqualified_spawn(&self.command)
                    .map_err(|error| io::Error::other(crate::FfmpegCommandError::from(error)))?;
                None
            }
        };
        FfmpegChild::spawn_owned(
            &mut self.command,
            None,
            deadline,
            #[cfg(feature = "validation")]
            lease,
        )
    }

    /// Execute a command and wait for its exit while retaining its authority.
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn()?.wait()
    }

    /// Capture output while retaining authority through the native wait.
    pub fn output(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self.spawn()?.wait_with_output()
    }
}

/// Native child retaining the qualified capsule independently of the command.
#[derive(Debug)]
pub struct FfmpegChild {
    child: Option<Child>,
    provider_owner: Option<std::sync::Arc<crate::approved_provider_command::ProviderOwner>>,
    #[cfg(windows)]
    job: crate::native_process_job::NativeProcessJob,
    #[cfg(target_os = "linux")]
    process_group: Option<linux_process_group::ProcessGroup>,
    #[cfg(feature = "validation")]
    lease: Option<crate::qualified_ffmpeg::QualifiedChildLease>,
}

impl FfmpegChild {
    pub(crate) fn spawn_owned(
        command: &mut Command,
        provider_owner: Option<std::sync::Arc<crate::approved_provider_command::ProviderOwner>>,
        deadline: Option<Instant>,
        #[cfg(feature = "validation")] lease: Option<crate::qualified_ffmpeg::QualifiedChildLease>,
    ) -> io::Result<Self> {
        check_spawn_deadline(deadline)?;
        #[cfg(windows)]
        let job = crate::native_process_job::NativeProcessJob::new()?;
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000 | 0x0000_0004);
        }
        check_spawn_deadline(deadline)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn()?;
        #[allow(unused_mut)]
        let mut owner = Self {
            #[cfg(target_os = "linux")]
            process_group: Some(linux_process_group::ProcessGroup::new(child.id())),
            child: Some(child),
            provider_owner,
            #[cfg(windows)]
            job,
            #[cfg(feature = "validation")]
            lease,
        };
        #[cfg(windows)]
        if let Some(child) = &owner.child
            && let Err(primary) = owner.job.assign_and_resume(child, deadline)
        {
            let cleanup = crate::process_supervisor::terminate_and_reap(
                &mut owner,
                deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(2)),
            );
            #[cfg(feature = "validation")]
            crate::qualified_ffmpeg::record_native_cleanup_failure(owner.id(), &cleanup);
            return Err(io::Error::new(
                primary.kind(),
                NativeProcessSpawnFailure { primary, cleanup },
            ));
        }
        #[cfg(not(windows))]
        if let Err(primary) = check_spawn_deadline(deadline) {
            let cleanup = crate::process_supervisor::terminate_and_reap(
                &mut owner,
                deadline.unwrap_or_else(Instant::now),
            );
            record_native_cleanup(owner.id(), &cleanup);
            return Err(io::Error::new(
                primary.kind(),
                NativeProcessSpawnFailure { primary, cleanup },
            ));
        }
        Ok(owner)
    }
    /// Wait for native exit before releasing the qualified child lease.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.stdin.take();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Observe native exit without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        #[cfg(target_os = "linux")]
        if let Some(group) = &self.process_group
            && !group.exited()?
        {
            return Ok(None);
        }
        let result = self.deref_mut().try_wait();
        if matches!(result, Ok(Some(_))) {
            #[cfg(windows)]
            if self.job.is_assigned() && self.job.active()? != 0 {
                return Ok(None);
            }
            #[cfg(target_os = "linux")]
            self.process_group.take();
            self.release_lease();
        }
        result
    }

    /// Terminate the entire owned native process tree, including launcher descendants.
    pub fn kill(&mut self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.process_group.as_ref().map_or(Ok(()), |group| group.terminate())
        }
        #[cfg(not(target_os = "linux"))]
        {
            #[cfg(windows)]
            if self.job.is_assigned() {
                return self.job.terminate();
            }
            self.deref_mut().kill()
        }
    }

    fn release_lease(&mut self) {
        self.provider_owner.take();
        #[cfg(feature = "validation")]
        self.lease.take();
    }

    /// Consume both native output pipes and wait for process termination.
    pub fn wait_with_output(self) -> io::Result<Output> {
        crate::process_supervisor::SupervisedChild::capture_native_output(self)
    }
}

impl Deref for FfmpegChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        self.child.as_ref().expect("native child is present until consuming wait")
    }
}

impl DerefMut for FfmpegChild {
    fn deref_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("native child is present until consuming wait")
    }
}

impl Drop for FfmpegChild {
    fn drop(&mut self) {
        let settled = self.child.is_some() && matches!(self.try_wait(), Ok(Some(_)));
        #[cfg(windows)]
        let settled = settled && matches!(self.job.active(), Ok(0));
        #[cfg(windows)]
        if !settled {
            let _ = self.job.terminate();
        }
        #[cfg(target_os = "linux")]
        if !settled
            && let (Some(group), Some(child)) = (self.process_group.take(), self.child.take())
        {
            group.retire(child);
        }
        if let Some(owner) = self.provider_owner.take() {
            if settled {
                drop(owner);
            } else {
                std::mem::forget(owner);
            }
        }
        #[cfg(feature = "validation")]
        if let Some(lease) = self.lease.take() {
            if settled {
                drop(lease);
            } else {
                // A lost native owner is not a release receipt. Retain the object
                // leases permanently and make consuming capsule closure fail.
                lease.abandon();
            }
        }
    }
}

pub(crate) mod sealed {
    pub trait Sealed {}
    impl Sealed for std::process::Command {}
    impl Sealed for super::FfmpegCommand {}
}

/// Commands accepted by the shared native process supervisor.
pub trait SupervisedCommand: sealed::Sealed {
    /// Configure the supervisor's three owned standard streams.
    fn configure_supervised_streams(&mut self, pipe_stdin: bool);
    /// Create a native process with its retained execution lease.
    fn spawn_supervised(&mut self) -> io::Result<FfmpegChild>;
    /// Carry the caller's original deadline through partial native spawn cleanup.
    fn spawn_supervised_until(&mut self, deadline: Option<Instant>) -> io::Result<FfmpegChild>;
    /// Capture output through the same spawn authority.
    fn capture_output(&mut self) -> io::Result<Output>;
}

impl SupervisedCommand for FfmpegCommand {
    fn configure_supervised_streams(&mut self, pipe_stdin: bool) {
        self.stdin(if pipe_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        self.hide_window();
    }
    fn spawn_supervised(&mut self) -> io::Result<FfmpegChild> {
        self.spawn()
    }
    fn spawn_supervised_until(&mut self, deadline: Option<Instant>) -> io::Result<FfmpegChild> {
        self.spawn_until(deadline)
    }
    fn capture_output(&mut self) -> io::Result<Output> {
        self.output()
    }
}

impl SupervisedCommand for Command {
    fn configure_supervised_streams(&mut self, pipe_stdin: bool) {
        self.stdin(if pipe_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x0800_0000);
        }
    }
    fn spawn_supervised(&mut self) -> io::Result<FfmpegChild> {
        spawn_native_helper(self)
    }
    fn spawn_supervised_until(&mut self, deadline: Option<Instant>) -> io::Result<FfmpegChild> {
        spawn_native_helper_until(self, deadline)
    }
    fn capture_output(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        spawn_native_helper(self)?.wait_with_output()
    }
}

pub(crate) fn spawn_native_helper(command: &mut Command) -> io::Result<FfmpegChild> {
    spawn_native_helper_until(command, None)
}

fn spawn_native_helper_until(
    command: &mut Command,
    deadline: Option<Instant>,
) -> io::Result<FfmpegChild> {
    #[cfg(feature = "validation")]
    crate::qualified_ffmpeg::reject_unattested_native_helper()
        .map_err(|error| io::Error::other(crate::FfmpegCommandError::from(error)))?;
    FfmpegChild::spawn_owned(
        command,
        None,
        deadline,
        #[cfg(feature = "validation")]
        None,
    )
}

#[derive(Debug, thiserror::Error)]
#[error("native process group admission failed: {primary}; cleanup: {cleanup:?}")]
pub(crate) struct NativeProcessSpawnFailure {
    #[source]
    primary: io::Error,
    pub(crate) cleanup: crate::SupervisedProcessCleanupReceipt,
}

#[derive(Debug, thiserror::Error)]
#[error("original native spawn deadline exceeded")]
struct NativeSpawnDeadlineExceeded;

pub(crate) fn check_spawn_deadline(deadline: Option<Instant>) -> io::Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            NativeSpawnDeadlineExceeded,
        ))
    } else {
        Ok(())
    }
}

/// Resolve a packaged native media helper with retained qualified spawn authority.
pub fn media_helper_command(path: &Path) -> io::Result<FfmpegCommand> {
    #[cfg(feature = "validation")]
    if let Some(owner) = crate::qualified_ffmpeg::process_toolchain() {
        return owner
            .native_helper_command(path)
            .map_err(|error| io::Error::other(crate::FfmpegCommandError::from(error)));
    }
    Ok(FfmpegCommand::new(path))
}

/// Exact pre-loader application image reused for approved native worker modes.
pub fn qualified_media_helper_path() -> Option<std::path::PathBuf> {
    #[cfg(all(windows, feature = "validation"))]
    return mondrian_validation_launcher::process_authority()
        .map(|authority| authority.application_path().to_path_buf());
    #[cfg(not(all(windows, feature = "validation")))]
    None
}

pub(crate) fn record_native_cleanup(pid: u32, cleanup: &crate::SupervisedProcessCleanupReceipt) {
    if cleanup.all_resources_released() {
        return;
    }
    #[cfg(feature = "validation")]
    crate::qualified_ffmpeg::record_native_cleanup_failure(pid, cleanup);
    tracing::error!(
        child_pid = pid,
        ?cleanup,
        "native media child consuming cleanup failed"
    );
}

#[cfg(target_os = "linux")]
mod linux_process_group {
    use std::io;
    use std::process::Child;
    use std::time::Duration;

    /// The unreaped leader pins the numeric process-group identity.
    #[derive(Debug)]
    pub(super) struct ProcessGroup {
        leader: u32,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("invalid Linux process-group inventory record")]
    struct InvalidProcessRecord;

    impl ProcessGroup {
        pub(super) fn new(leader: u32) -> Self {
            Self { leader }
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // Linux PIDs fit pid_t. The leader has not been reaped, so this
            // group number cannot have been recycled for an unrelated owner.
            if unsafe { libc::kill(-(self.leader as libc::pid_t), libc::SIGKILL) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn exited(&self) -> io::Result<bool> {
            let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // Observe without reaping: an exiting launcher must not release
            // its group identity or executable lease while descendants run.
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.leader,
                    &mut status,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if unsafe { status.si_pid() } == 0 {
                return Ok(false);
            }
            for entry in std::fs::read_dir("/proc")? {
                let entry = entry?;
                let Some(pid) =
                    entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok())
                else {
                    continue;
                };
                if pid == self.leader {
                    continue;
                }
                let stat = match std::fs::read(entry.path().join("stat")) {
                    Ok(stat) => stat,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                // comm may contain spaces, parentheses and non-UTF8 bytes.
                let end = stat
                    .iter()
                    .rposition(|byte| *byte == b')')
                    .ok_or_else(|| io::Error::other(InvalidProcessRecord))?;
                let fields = std::str::from_utf8(&stat[end + 1..])
                    .map_err(|_| io::Error::other(InvalidProcessRecord))?;
                let mut fields = fields.split_whitespace();
                let state = fields.next();
                let _parent = fields.next();
                let group = fields
                    .next()
                    .and_then(|field| field.parse::<u32>().ok())
                    .ok_or_else(|| io::Error::other(InvalidProcessRecord))?;
                if group == self.leader && !matches!(state, Some("Z" | "X")) {
                    return Ok(false);
                }
            }
            Ok(true)
        }

        pub(super) fn retire(self, mut child: Child) {
            if let Err(error) = self.terminate() {
                tracing::error!(pid = self.leader, %error, "native process group termination failed");
            }
            // A deadline failure stays failed. This last-resort reaper owns the
            // same native child; it cannot publish or upgrade a closure receipt.
            let pid = self.leader;
            let result = std::thread::Builder::new()
                .name("mondrian-media-native-reap".to_owned())
                .spawn(move || {
                    loop {
                        match self.exited() {
                            Ok(true) => break,
                            Ok(false) => std::thread::sleep(Duration::from_millis(1)),
                            Err(error) => {
                                tracing::error!(pid, %error, "native process group exit observation failed");
                                break;
                            }
                        }
                    }
                    if let Err(error) = child.wait() {
                        tracing::error!(pid, %error, "native process leader reap failed");
                    }
                });
            if let Err(error) = result {
                tracing::error!(pid, %error, "native process reaper creation failed");
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_lifecycle_tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn exiting_launcher_does_not_release_its_live_descendant() {
        let mut command = FfmpegCommand::new("/bin/sh");
        command
            .args(["-c", "sleep 30 & printf '%s\\n' $!; exit 7"])
            .stdout(Stdio::piped());
        let mut child = command.spawn().expect("spawn exiting launcher");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("PID pipe"))
            .read_line(&mut line)
            .expect("read descendant PID");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id()))
                .expect("unreaped launcher identity");
            if stat.rsplit_once(") ").is_some_and(|(_, fields)| fields.starts_with('Z')) {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("terminate timed out launcher");
                child.wait().expect("reap timed out launcher");
                panic!("launcher did not exit");
            }
            std::thread::yield_now();
        }
        let observed = child.try_wait().expect("observe owned group");
        child.kill().expect("terminate descendant after launcher exit");
        let status = child.wait().expect("reap group");
        assert!(
            observed.is_none(),
            "live descendant was reported as native exit"
        );
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn output_capture_keeps_nonzero_status_and_drains_both_pipes() {
        let mut command = FfmpegCommand::new("/bin/sh");
        command.args(["-c", "printf out; printf err >&2; exit 7"]);
        let output = command.output().expect("capture through production supervisor");
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[test]
    fn terminating_media_child_also_terminates_pipe_holding_descendant() {
        let mut command = FfmpegCommand::new("/bin/sh");
        command
            .args(["-c", "trap '' TERM; sleep 30 & printf '%s\\n' $!; wait"])
            .stdout(Stdio::piped());
        let mut child = command.spawn().expect("spawn media helper");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("PID pipe"))
            .read_line(&mut line)
            .expect("read descendant PID");
        let pid: i32 = line.trim().parse().expect("descendant PID");
        child.kill().expect("terminate media owner");
        child.wait().expect("reap media owner");
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
        let alive = stat.as_ref().is_ok_and(|stat| {
            stat.rsplit_once(") ").is_some_and(|(_, fields)| !fields.starts_with('Z'))
        });
        // Clean the deliberately exposed descendant before asserting the regression.
        if alive {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(
            !alive,
            "the media owner's pipe-holding descendant survived kill/wait"
        );
    }

    #[test]
    fn expired_native_spawn_deadline_rejects_before_creating_child() {
        let mut command = FfmpegCommand::new("/bin/true");
        let result = command.spawn_until(Some(Instant::now()));
        let error = match result {
            Ok(mut child) => {
                child.wait().expect("reap unexpectedly admitted child");
                panic!("expired native spawn deadline admitted a child");
            }
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}

#[cfg(all(test, windows))]
mod native_job_tests {
    use super::*;
    use crate::{run_supervised_command, SupervisedProcessError, SupervisedProcessPolicy};
    use mondrian_core::ExecutionCancellationToken;

    const MODE: &str = "MONDRIAN_NATIVE_JOB_DESCENDANT_TEST_MODE";
    const PID_FILE: &str = "MONDRIAN_NATIVE_JOB_DESCENDANT_TEST_PID_FILE";

    #[test]
    #[allow(
        clippy::zombie_processes,
        reason = "This child fixture models an exiting shim; the outer test owns and reaps its inherited native Job."
    )]
    fn native_job_child_entry() {
        let Ok(mode) = std::env::var(MODE) else {
            return;
        };
        if mode == "descendant" {
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        assert_eq!(mode, "exiting-shim");
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "ffmpeg_command::native_job_tests::native_job_child_entry",
                "--nocapture",
            ])
            .env(MODE, "descendant")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let child = command.spawn().expect("actual raw native descendant inherits parent job");
        std::fs::write(
            std::env::var_os(PID_FILE).expect("PID receipt"),
            child.id().to_string(),
        )
        .expect("publish exact descendant PID");
        // The shim exits immediately; its descendant retains both output pipes.
        // std Child intentionally has no kill-on-drop behavior in this child fixture.
        drop(child);
    }

    #[test]
    fn canceled_exiting_shim_reaps_its_actual_descendant_and_closes_inherited_pipes() {
        let root = tempfile::tempdir().expect("PID receipt root");
        let pid_file = root.path().join("descendant-pid.txt");
        let mut command = FfmpegCommand::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "ffmpeg_command::native_job_tests::native_job_child_entry",
                "--nocapture",
            ])
            .env(MODE, "exiting-shim")
            .env(PID_FILE, &pid_file);
        let cancellation = ExecutionCancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        let watcher_token = cancellation.clone();
        let watcher_path = pid_file.clone();
        let watcher = std::thread::spawn(move || {
            while !watcher_path.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            watcher_token.cancel();
        });
        let result = run_supervised_command(
            &mut command,
            None,
            SupervisedProcessPolicy {
                deadline: Some(deadline),
                ..SupervisedProcessPolicy::default()
            },
            &cancellation,
        );
        watcher.join().expect("bounded cancellation observer");
        let error = result.expect_err("root-only exit must not claim the process tree ended");
        assert!(error.is_canceled(), "{error:?}");
        let SupervisedProcessError::Cleanup { cleanup, .. } = error else {
            panic!("raw native cleanup missing")
        };
        assert!(cleanup.all_resources_released(), "{cleanup:?}");
        let pid: u32 = std::fs::read_to_string(pid_file)
            .expect("actual descendant PID")
            .parse()
            .expect("PID");
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if !raw.is_null() {
            let _process = unsafe { std::fs::File::from_raw_handle(raw) };
            assert_eq!(
                unsafe { WaitForSingleObject(raw, 0) },
                0,
                "owned descendant must actually be exited"
            );
        } else {
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(87),
                "only an already-gone process proves exit"
            );
        }
    }
}
