//! BMX executable authority sharing the approved provider capsule and child ledger.
use crate::approved_provider_command::{boundary, prepare_provider_runtime, ProviderOwner};
use crate::{ApprovedProviderFile, ApprovedProviderRuntimeCleanupReceipt, SupervisedCommand};
use mondrian_core::ExecutionCancellationToken;
use std::{
    ffi::OsStr,
    io,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Instant,
};

/// The only executable roles admitted by a BMX runtime owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovedBmxTool {
    /// BMX raw essence wrapper.
    Raw2Bmx,
    /// Independent BMX MXF reader.
    Mxf2Raw,
}

/// Consuming owner of approved BMX executable objects and the complete DLL closure.
#[derive(Debug)]
pub struct PreparedBmxRuntime {
    handle: BmxRuntimeHandle,
}

/// A phase-bounded borrowing authority; the consuming owner must outlive all copies.
#[derive(Debug, Clone)]
pub struct BmxRuntimeHandle {
    owner: Arc<ProviderOwner>,
    deadline: Instant,
    cancellation: ExecutionCancellationToken,
}

impl PartialEq for BmxRuntimeHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner) && self.deadline == other.deadline
    }
}
impl Eq for BmxRuntimeHandle {}

/// Prepare exact raw2bmx/mxf2raw owners, in that order, with approved runtime DLLs.
/// A returned `None` means a qualified process lacked the declared loader closure.
/// Ordinary development may omit `runtime_files` and retain exact source objects
/// without claiming a sealed namespace; campaigns require `Some` explicitly.
pub fn prepare_bmx_runtime(
    approved: [ApprovedProviderFile; 2],
    runtime_files: Option<Vec<ApprovedProviderFile>>,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> io::Result<Option<PreparedBmxRuntime>> {
    prepare_bmx_runtime_for_phase(approved, runtime_files, deadline, deadline, cancellation)
}

/// Freeze separate original preparation and phase-owner horizons before admission.
/// Namespace copying/sealing uses the startup limit; a newly constructed handle
/// may serve the subsequent measured phase without renewing any running command.
pub fn prepare_bmx_runtime_for_phase(
    approved: [ApprovedProviderFile; 2],
    runtime_files: Option<Vec<ApprovedProviderFile>>,
    preparation_deadline: Instant,
    owner_deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> io::Result<Option<PreparedBmxRuntime>> {
    if preparation_deadline > owner_deadline {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "BMX startup horizon exceeds owner horizon",
        ));
    }
    prepare_provider_runtime(
        Vec::from(approved),
        runtime_files,
        1,
        preparation_deadline,
        cancellation,
    )
    .map(|owner| {
        owner.map(|owner| PreparedBmxRuntime {
            handle: BmxRuntimeHandle {
                owner,
                deadline: owner_deadline,
                cancellation: cancellation.clone(),
            },
        })
    })
}

impl PreparedBmxRuntime {
    /// Borrow this phase's authority for immutable Export requests.
    pub fn handle(&self) -> BmxRuntimeHandle {
        self.handle.clone()
    }

    /// Consume the namespace only after all requests, commands and children release it.
    pub fn close_until(self, deadline: Instant) -> ApprovedProviderRuntimeCleanupReceipt {
        let receipt =
            ProviderOwner::consuming_close(self.handle.owner, deadline.min(self.handle.deadline));
        #[cfg(feature = "validation")]
        if !receipt.all_resources_released() {
            crate::qualified_ffmpeg::record_external_provider_cleanup_failure(&format!(
                "BMX runtime cleanup: {receipt:?}"
            ));
        }
        receipt
    }
}

impl BmxRuntimeHandle {
    /// Exact retained executable path; approved commands never use PATH discovery.
    pub fn path(&self, tool: ApprovedBmxTool) -> &Path {
        match tool {
            ApprovedBmxTool::Raw2Bmx => &self.owner.executable,
            ApprovedBmxTool::Mxf2Raw => &self.owner.profile,
        }
    }

    /// Construct one executable-bound command. Only argv can subsequently change.
    pub fn command(&self, tool: ApprovedBmxTool) -> ApprovedBmxCommand {
        let mut command = Command::new(self.path(tool));
        let preparation_error = self.owner.configure_command(&mut command).err();
        ApprovedBmxCommand {
            command,
            authority: Some(self.clone()),
            preparation_error,
        }
    }
}

/// Supervisor-only BMX command; executable and loader environment cannot escape its owner.
#[derive(Debug)]
pub struct ApprovedBmxCommand {
    command: Command,
    authority: Option<BmxRuntimeHandle>,
    preparation_error: Option<io::Error>,
}

impl ApprovedBmxCommand {
    /// Ordinary development execution; existing supervisor admission rejects this in campaigns.
    pub fn unapproved(path: &Path) -> Self {
        Self {
            command: Command::new(path),
            authority: None,
            preparation_error: None,
        }
    }
    /// Append a tool protocol argument without access to executable/env/native spawn.
    pub fn arg(&mut self, value: impl AsRef<OsStr>) -> &mut Self {
        self.command.arg(value);
        self
    }
    /// Append tool protocol arguments without changing native execution authority.
    pub fn args<I, S>(&mut self, values: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(values);
        self
    }
    /// Inspect exact protocol arguments for tests and diagnostics.
    pub fn get_args(&self) -> std::process::CommandArgs<'_> {
        self.command.get_args()
    }
    /// Inspect the exact executable backed by this command.
    pub fn get_program(&self) -> &OsStr {
        self.command.get_program()
    }
}

impl crate::ffmpeg_command::sealed::Sealed for ApprovedBmxCommand {}
impl SupervisedCommand for ApprovedBmxCommand {
    fn configure_supervised_streams(&mut self, pipe_stdin: bool) {
        self.command
            .stdin(if pipe_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }
    fn spawn_supervised(&mut self) -> io::Result<crate::FfmpegChild> {
        self.spawn_supervised_until(None)
    }
    fn spawn_supervised_until(
        &mut self,
        deadline: Option<Instant>,
    ) -> io::Result<crate::FfmpegChild> {
        if let Some(error) = &self.preparation_error {
            return Err(io::Error::new(error.kind(), error.to_string()));
        }
        let Some(authority) = &self.authority else {
            return self.command.spawn_supervised_until(deadline);
        };
        let deadline = deadline.map_or(authority.deadline, |value| value.min(authority.deadline));
        authority.owner.validate(deadline, &authority.cancellation)?;
        #[cfg(feature = "validation")]
        let lease = crate::qualified_ffmpeg::process_toolchain()
            .map(|owner| owner.admit_provider_child())
            .transpose()
            .map_err(io::Error::other)?;
        boundary(deadline, &authority.cancellation)?;
        crate::FfmpegChild::spawn_owned(
            &mut self.command,
            Some(Arc::clone(&authority.owner)),
            Some(deadline),
            #[cfg(feature = "validation")]
            lease,
        )
    }
    fn capture_output(&mut self) -> io::Result<std::process::Output> {
        Err(io::Error::other(
            "BMX requires bounded SupervisedChild execution with raw cleanup evidence",
        ))
    }
}
