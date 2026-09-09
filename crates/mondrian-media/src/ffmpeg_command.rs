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
        #[cfg(windows)]
        crate::native_process_job::check_spawn_deadline(deadline)?;
        #[cfg(windows)]
        let job = crate::native_process_job::NativeProcessJob::new()?;
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000 | 0x0000_0004);
        }
        #[cfg(windows)]
        crate::native_process_job::check_spawn_deadline(deadline)?;
        let child = command.spawn()?;
        #[allow(unused_mut)]
        let mut owner = Self {
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
        let _ = deadline;
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
        let result = self.deref_mut().try_wait();
        if matches!(result, Ok(Some(_))) {
            #[cfg(windows)]
            if self.job.is_assigned() && self.job.active()? != 0 {
                return Ok(None);
            }
            self.release_lease();
        }
        result
    }

    /// Terminate the entire owned native process tree, including launcher descendants.
    pub fn kill(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        if self.job.is_assigned() {
            return self.job.terminate();
        }
        self.deref_mut().kill()
    }

    fn release_lease(&mut self) {
        self.provider_owner.take();
        #[cfg(feature = "validation")]
        self.lease.take();
    }

    /// Consume both native output pipes and wait for process termination.
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        let Some(child) = self.child.take() else {
            return Err(io::Error::other("native media child already consumed"));
        };
        let result = child.wait_with_output().and_then(|output| {
            #[cfg(windows)]
            while self.job.active()? != 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(output)
        });
        if result.is_ok() {
            self.release_lease();
        } else {
            if let Some(owner) = self.provider_owner.take() {
                std::mem::forget(owner);
            }
            #[cfg(feature = "validation")]
            if let Some(lease) = self.lease.take() {
                lease.abandon();
            }
        }
        result
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
        let settled =
            self.child.as_mut().is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))));
        #[cfg(windows)]
        let settled = settled && matches!(self.job.active(), Ok(0));
        #[cfg(windows)]
        if !settled {
            let _ = self.job.terminate();
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

#[cfg(windows)]
#[derive(Debug, thiserror::Error)]
#[error("native process group admission failed: {primary}; cleanup: {cleanup:?}")]
pub(crate) struct NativeProcessSpawnFailure {
    #[source]
    primary: io::Error,
    pub(crate) cleanup: crate::SupervisedProcessCleanupReceipt,
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
