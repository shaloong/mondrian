//! Narrow external-provider authority; a raw Command cannot carry these owners.
use mondrian_core::ExecutionCancellationToken;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::Instant,
};

const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Raw consuming closure of one approved executable namespace and its file leases.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedProviderRuntimeCleanupReceipt {
    /// No command, handle or native child retained the owner at consuming close.
    pub commands_released: bool,
    /// A sealed private namespace was owned for this execution.
    pub namespace_owned: bool,
    /// Actual seal validation error, if that stage failed.
    pub namespace_validation_error: Option<String>,
    /// Actual original ACL restoration error, if that stage failed.
    pub namespace_restore_error: Option<String>,
    /// Actual exact temporary-directory removal error, if that stage failed.
    pub namespace_remove_error: Option<String>,
    /// Every source and staged file lease was released by the owner.
    pub file_leases_released: bool,
    /// The original close deadline had expired at completion.
    pub deadline_exceeded: bool,
    /// Retained command/child owners prevented consuming closure.
    pub outstanding_owner_error: Option<String>,
}

impl ApprovedProviderRuntimeCleanupReceipt {
    /// True only when actual consuming operations closed the complete owner.
    pub fn all_resources_released(&self) -> bool {
        self.commands_released
            && self.file_leases_released
            && !self.deadline_exceeded
            && self.namespace_validation_error.is_none()
            && self.namespace_restore_error.is_none()
            && self.namespace_remove_error.is_none()
            && self.outstanding_owner_error.is_none()
    }
}

/// An externally approved file together with its already-retained native object.
/// Approval semantics belong to the provider Adapter, never to this transport.
#[derive(Debug)]
pub struct ApprovedProviderFile {
    path: PathBuf,
    sha256: [u8; 32],
    file: File,
}
impl ApprovedProviderFile {
    /// Consume an approved file lease. Preparation independently pins the same
    /// direct object and verifies all bytes before this can authorize a command.
    pub fn from_retained(path: PathBuf, sha256: [u8; 32], file: File) -> Self {
        Self { path, sha256, file }
    }
}

/// Admission is explicit before phase startup when the approved loader closure is absent.
#[derive(Debug)]
pub enum RegulatoryPseRuntimeAdmission {
    /// Native files and, for qualified execution, a sealed loader namespace are owned.
    Available(PreparedRegulatoryPseRuntime),
    /// Qualified execution requires an explicitly approved complete DLL closure.
    RuntimeClosureMissing,
}

/// Prepared narrow protocol authority, consumed after native process settlement.
#[derive(Debug)]
pub struct PreparedRegulatoryPseRuntime {
    owner: Arc<ProviderOwner>,
}

#[derive(Debug)]
pub(crate) struct ProviderOwner {
    pub(crate) executable: PathBuf,
    pub(crate) profile: PathBuf,
    #[cfg_attr(not(all(windows, feature = "validation")), allow(dead_code))]
    profile_index: usize,
    files: Mutex<Vec<ApprovedProviderFile>>,
    #[cfg(all(windows, feature = "validation"))]
    capsule: Option<ProviderCapsule>,
}

#[cfg(all(windows, feature = "validation"))]
#[derive(Debug)]
struct ProviderCapsule {
    directory: tempfile::TempDir,
    seal: mondrian_validation_launcher::namespace::CapsuleSeal,
    ancestors: Vec<File>,
}

/// Prepare the fixed regulatory PSE protocol from executable, approval-document
/// and approved-profile owners, in that order. Runtime entries are approved DLLs.
pub fn prepare_regulatory_pse_runtime(
    approved: [ApprovedProviderFile; 3],
    runtime_files: Option<Vec<ApprovedProviderFile>>,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> io::Result<RegulatoryPseRuntimeAdmission> {
    prepare_provider_runtime(
        Vec::from(approved),
        runtime_files,
        2,
        deadline,
        cancellation,
    )
    .map(|owner| match owner {
        Some(owner) => {
            RegulatoryPseRuntimeAdmission::Available(PreparedRegulatoryPseRuntime { owner })
        }
        None => RegulatoryPseRuntimeAdmission::RuntimeClosureMissing,
    })
}

pub(crate) fn prepare_provider_runtime(
    mut files: Vec<ApprovedProviderFile>,
    runtime_files: Option<Vec<ApprovedProviderFile>>,
    profile_index: usize,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> io::Result<Option<Arc<ProviderOwner>>> {
    let qualified = qualified_process();
    let prepare_namespace =
        qualified || (runtime_files.is_some() && cfg!(all(windows, feature = "validation")));
    if qualified && runtime_files.is_none() {
        return Ok(None);
    }
    boundary(deadline, cancellation)?;
    if let Some(runtime) = runtime_files {
        if runtime.len() > 256 {
            return Err(io::Error::other(
                "provider runtime closure exceeds 256 files",
            ));
        }
        for file in &runtime {
            if !file.path.extension().is_some_and(|value| value.eq_ignore_ascii_case("dll")) {
                return Err(io::Error::other(
                    "provider runtime closure must contain direct DLL files",
                ));
            }
        }
        files.extend(runtime);
    }
    let mut total = 0_u64;
    for approved in &mut files {
        pin_and_verify(approved, deadline, cancellation)?;
        total = total
            .checked_add(approved.file.metadata()?.len())
            .ok_or_else(|| io::Error::other("provider closure size overflow"))?;
        if total > MAX_TOTAL_BYTES {
            return Err(io::Error::other("provider runtime exceeds eight GiB"));
        }
    }
    #[cfg_attr(not(all(windows, feature = "validation")), allow(unused_mut))]
    let mut owner = ProviderOwner {
        executable: files[0].path.clone(),
        profile: files[profile_index].path.clone(),
        profile_index,
        files: Mutex::new(files),
        #[cfg(all(windows, feature = "validation"))]
        capsule: None,
    };
    #[cfg(all(windows, feature = "validation"))]
    if prepare_namespace {
        owner.prepare_capsule(deadline, cancellation)?;
    }
    #[cfg(not(all(windows, feature = "validation")))]
    if prepare_namespace {
        return Err(io::Error::other(
            "native provider capsule authority unavailable",
        ));
    }
    boundary(deadline, cancellation)?;
    Ok(Some(Arc::new(owner)))
}

impl PreparedRegulatoryPseRuntime {
    /// Profile pathname backed by the same immutable owner used for execution.
    pub fn approved_profile_path(&self) -> &Path {
        &self.owner.profile
    }

    /// Build the single fixed protocol command; executable/argv/env are not mutable.
    pub fn command(
        &self,
        deadline: Instant,
        cancellation: &ExecutionCancellationToken,
    ) -> io::Result<ApprovedRegulatoryPseCommand> {
        self.owner.validate(deadline, cancellation)?;
        let mut command = Command::new(&self.owner.executable);
        command.arg("--mondrian-regulatory-pse-v1");
        #[cfg(all(windows, feature = "validation"))]
        if let Some(capsule) = &self.owner.capsule {
            let root = capsule.directory.path();
            let system =
                crate::qualified_ffmpeg::windows_system_directories().map_err(io::Error::other)?;
            let windows = system.last().ok_or_else(|| io::Error::other("Windows root absent"))?;
            command
                .env_clear()
                .current_dir(root)
                .env("PATH", root)
                .env("SystemRoot", windows);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        Ok(ApprovedRegulatoryPseCommand {
            command,
            owner: Arc::clone(&self.owner),
            deadline,
            cancellation: cancellation.clone(),
        })
    }

    /// Consume exact namespace and file owners. A live/abandoned native child
    /// retains its Arc, so this cannot claim successful disposal prematurely.
    pub fn close(self, deadline: Instant) -> io::Result<()> {
        let result = match Arc::try_unwrap(self.owner) {
            Ok(owner) => owner.close(deadline),
            Err(owner) => {
                std::mem::forget(owner);
                Err(io::Error::other(
                    "provider command or native child still retains its runtime owner",
                ))
            }
        };
        #[cfg(feature = "validation")]
        if let Err(error) = &result {
            crate::qualified_ffmpeg::record_external_provider_cleanup_failure(&error.to_string());
        }
        result
    }
}

/// Supervisor-only fixed regulatory protocol command carrying native authority.
#[derive(Debug)]
pub struct ApprovedRegulatoryPseCommand {
    command: Command,
    owner: Arc<ProviderOwner>,
    deadline: Instant,
    cancellation: ExecutionCancellationToken,
}

impl crate::ffmpeg_command::sealed::Sealed for ApprovedRegulatoryPseCommand {}
impl crate::SupervisedCommand for ApprovedRegulatoryPseCommand {
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
        self.spawn_supervised_until(Some(self.deadline))
    }
    fn spawn_supervised_until(
        &mut self,
        deadline: Option<Instant>,
    ) -> io::Result<crate::FfmpegChild> {
        let deadline = deadline.map_or(self.deadline, |deadline| deadline.min(self.deadline));
        self.owner.validate(deadline, &self.cancellation)?;
        #[cfg(feature = "validation")]
        let lease = crate::qualified_ffmpeg::process_toolchain()
            .map(|owner| owner.admit_provider_child())
            .transpose()
            .map_err(io::Error::other)?;
        boundary(deadline, &self.cancellation)?;
        crate::FfmpegChild::spawn_owned(
            &mut self.command,
            Some(Arc::clone(&self.owner)),
            Some(deadline),
            #[cfg(feature = "validation")]
            lease,
        )
    }
    fn capture_output(&mut self) -> io::Result<std::process::Output> {
        self.configure_supervised_streams(false);
        self.spawn_supervised()?.wait_with_output()
    }
}

impl ProviderOwner {
    pub(crate) fn consuming_close(
        owner: Arc<Self>,
        deadline: Instant,
    ) -> ApprovedProviderRuntimeCleanupReceipt {
        match Arc::try_unwrap(owner) {
            Ok(mut owner) => owner.close_resources_receipt(deadline),
            Err(owner) => {
                let mut receipt = owner.empty_cleanup_receipt(deadline);
                receipt.commands_released = false;
                receipt.outstanding_owner_error = Some(
                    "provider handle, command or native child still retains its runtime owner"
                        .to_owned(),
                );
                std::mem::forget(owner);
                receipt
            }
        }
    }

    fn empty_cleanup_receipt(&self, deadline: Instant) -> ApprovedProviderRuntimeCleanupReceipt {
        ApprovedProviderRuntimeCleanupReceipt {
            commands_released: true,
            namespace_owned: {
                #[cfg(all(windows, feature = "validation"))]
                {
                    self.capsule.is_some()
                }
                #[cfg(not(all(windows, feature = "validation")))]
                {
                    false
                }
            },
            namespace_validation_error: None,
            namespace_restore_error: None,
            namespace_remove_error: None,
            file_leases_released: false,
            deadline_exceeded: Instant::now() >= deadline,
            outstanding_owner_error: None,
        }
    }
    pub(crate) fn configure_command(&self, command: &mut Command) -> io::Result<()> {
        #[cfg(all(windows, feature = "validation"))]
        if let Some(capsule) = &self.capsule {
            let root = capsule.directory.path();
            let system =
                crate::qualified_ffmpeg::windows_system_directories().map_err(io::Error::other)?;
            let windows = system.last().ok_or_else(|| io::Error::other("Windows root absent"))?;
            command
                .env_clear()
                .current_dir(root)
                .env("PATH", root)
                .env("SystemRoot", windows);
        }
        #[cfg(not(all(windows, feature = "validation")))]
        let _ = command;
        Ok(())
    }

    #[cfg(all(windows, feature = "validation"))]
    fn prepare_capsule(
        &mut self,
        deadline: Instant,
        cancellation: &ExecutionCancellationToken,
    ) -> io::Result<()> {
        self.prepare_capsule_after_seal(deadline, cancellation, |_| Ok(()))
    }

    #[cfg(all(windows, feature = "validation"))]
    fn prepare_capsule_after_seal(
        &mut self,
        deadline: Instant,
        cancellation: &ExecutionCancellationToken,
        after_seal: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        use std::collections::BTreeSet;
        use std::io::Write;
        let parent = std::env::temp_dir().canonicalize()?;
        let mut ancestry = BTreeSet::new();
        ancestry.extend(parent.ancestors().map(Path::to_path_buf));
        for file in self.files.get_mut().iter() {
            let source = file.path.canonicalize()?;
            if let Some(parent) = source.parent() {
                ancestry.extend(parent.ancestors().map(Path::to_path_buf));
            }
        }
        let ancestors = ancestry
            .iter()
            .map(|path| crate::qualified_ffmpeg::open_direct_read_directory(path))
            .collect::<io::Result<Vec<_>>>()?;
        let directory =
            tempfile::Builder::new().prefix("mondrian-approved-pse-").tempdir_in(parent)?;
        let mut staged = Vec::new();
        let mut retained_seal = None;
        let preparation = (|| -> io::Result<_> {
            let mut names = BTreeSet::new();
            for (index, source) in self.files.get_mut().iter_mut().enumerate() {
                boundary(deadline, cancellation)?;
                let name = match index {
                    0 => "provider.exe".to_owned(),
                    1 => "approval-document.bin".to_owned(),
                    2 => "approved-profile.bin".to_owned(),
                    _ => source
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| io::Error::other("provider DLL filename is not Unicode"))?
                        .to_owned(),
                };
                if !names.insert(name.to_ascii_lowercase()) {
                    return Err(io::Error::other("duplicate provider capsule filename"));
                }
                let path = directory.path().join(name);
                let mut output =
                    std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
                source.file.seek(SeekFrom::Start(0))?;
                let mut bytes = [0_u8; 64 * 1024];
                loop {
                    boundary(deadline, cancellation)?;
                    let count = source.file.read(&mut bytes)?;
                    boundary(deadline, cancellation)?;
                    if count == 0 {
                        break;
                    }
                    output.write_all(&bytes[..count])?;
                }
                output.flush()?;
                drop(output);
                let mut file = ApprovedProviderFile {
                    file: strict_read(&path)?,
                    path,
                    sha256: source.sha256,
                };
                verify_file(&mut file, deadline, cancellation)?;
                staged.push(file);
            }
            retained_seal = Some(mondrian_validation_launcher::namespace::CapsuleSeal::seal(
                directory.path(),
            )?);
            after_seal(directory.path())?;
            let entries = std::fs::read_dir(directory.path())?.collect::<io::Result<Vec<_>>>()?;
            if entries.len() != names.len()
                || entries.iter().any(|entry| {
                    !entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| names.contains(&name.to_ascii_lowercase()))
                })
            {
                return Err(io::Error::other(
                    "unapproved file entered provider namespace before sealing",
                ));
            }
            for file in &mut staged {
                verify_file(file, deadline, cancellation)?;
            }
            boundary(deadline, cancellation)?;
            Ok(())
        })();
        match preparation {
            Ok(()) => {
                let seal = retained_seal.take().ok_or_else(|| {
                    io::Error::other("provider capsule preparation omitted its namespace owner")
                })?;
                self.executable = staged[0].path.clone();
                self.profile = staged[self.profile_index].path.clone();
                self.files.get_mut().extend(staged);
                self.capsule = Some(ProviderCapsule { directory, seal, ancestors });
                Ok(())
            }
            Err(primary) => {
                let restoration = retained_seal.as_mut().map(|seal| seal.restore()).transpose();
                drop(retained_seal);
                drop(staged);
                let removal = directory.close();
                if restoration.is_ok() && removal.is_ok() {
                    return Err(primary);
                }
                let mut errors = vec![format!("provider namespace preparation: {primary}")];
                if let Err(error) = restoration {
                    errors.push(format!("provider namespace restoration: {error}"));
                }
                if let Err(error) = removal {
                    errors.push(format!("provider exact directory removal: {error}"));
                }
                Err(io::Error::other(errors.join("; ")))
            }
        }
    }

    pub(crate) fn validate(
        &self,
        deadline: Instant,
        cancellation: &ExecutionCancellationToken,
    ) -> io::Result<()> {
        boundary(deadline, cancellation)?;
        #[cfg(all(windows, feature = "validation"))]
        if let Some(capsule) = &self.capsule {
            capsule.seal.validate()?;
        }
        if qualified_process() {
            #[cfg(all(windows, feature = "validation"))]
            if self.capsule.is_none() {
                return Err(io::Error::other(
                    "provider has no qualified namespace owner",
                ));
            }
        }
        let mut files = self.files.try_lock_until(deadline).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "original provider file lease deadline exceeded",
            )
        })?;
        boundary(deadline, cancellation)?;
        for file in files.iter_mut() {
            verify_file(file, deadline, cancellation)?;
        }
        boundary(deadline, cancellation)
    }

    pub(crate) fn close(mut self, deadline: Instant) -> io::Result<()> {
        self.close_resources(deadline)
    }

    fn close_resources(&mut self, deadline: Instant) -> io::Result<()> {
        let receipt = self.close_resources_receipt(deadline);
        if receipt.all_resources_released() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "approved provider consuming cleanup: {receipt:?}"
            )))
        }
    }

    fn close_resources_receipt(
        &mut self,
        deadline: Instant,
    ) -> ApprovedProviderRuntimeCleanupReceipt {
        let mut receipt = self.empty_cleanup_receipt(deadline);
        #[cfg(all(windows, feature = "validation"))]
        if let Some(mut capsule) = self.capsule.take() {
            if let Err(error) = capsule.seal.validate() {
                receipt.namespace_validation_error = Some(error.to_string());
            }
            if let Err(error) = capsule.seal.restore() {
                receipt.namespace_restore_error = Some(error.to_string());
            }
            drop(capsule.seal);
            self.files.get_mut().clear();
            if let Err(error) = capsule.directory.close() {
                receipt.namespace_remove_error = Some(error.to_string());
            }
            drop(capsule.ancestors);
        }
        self.files.get_mut().clear();
        receipt.file_leases_released = true;
        receipt.deadline_exceeded = Instant::now() >= deadline;
        receipt
    }
}

impl Drop for ProviderOwner {
    fn drop(&mut self) {
        if self.files.get_mut().is_empty() {
            return;
        }
        if let Err(error) = self.close_resources(Instant::now() + std::time::Duration::from_secs(5))
        {
            #[cfg(feature = "validation")]
            crate::qualified_ffmpeg::record_external_provider_cleanup_failure(&error.to_string());
            tracing::error!(%error, "external provider fallback runtime closure failed");
        }
    }
}

pub(crate) fn qualified_process() -> bool {
    #[cfg(feature = "validation")]
    {
        crate::qualified_ffmpeg::process_toolchain().is_some()
    }
    #[cfg(not(feature = "validation"))]
    {
        false
    }
}

/// Exact owner-token cancellation observed during provider preparation.
#[derive(Debug, thiserror::Error)]
#[error("provider preparation canceled")]
pub(crate) struct ProviderPreparationCanceled;

pub(crate) fn boundary(
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> io::Result<()> {
    if cancellation.is_canceled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            ProviderPreparationCanceled,
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "original provider deadline exceeded",
        ));
    }
    Ok(())
}

fn strict_read(path: &Path) -> io::Result<File> {
    if !path.is_absolute() || !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(io::Error::other(
            "provider approval requires an absolute direct regular object",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x0020_0000);
    }
    options.open(path)
}

fn pin_and_verify(
    approved: &mut ApprovedProviderFile,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> io::Result<()> {
    let pinned = strict_read(&approved.path)?;
    if native_identity(&pinned)? != native_identity(&approved.file)? {
        return Err(io::Error::other(
            "approved provider lease no longer names the same native object",
        ));
    }
    approved.file = pinned;
    verify_file(approved, deadline, cancellation)
}

fn verify_file(
    approved: &mut ApprovedProviderFile,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> io::Result<()> {
    let fresh = strict_read(&approved.path)?;
    if native_identity(&fresh)? != native_identity(&approved.file)? {
        return Err(io::Error::other("provider native object changed"));
    }
    approved.file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        boundary(deadline, cancellation)?;
        let count = approved.file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_FILE_BYTES {
            return Err(io::Error::other("provider file exceeds admitted size"));
        }
        hash.update(&bytes[..count]);
    }
    boundary(deadline, cancellation)?;
    if total == 0 || <[u8; 32]>::from(hash.finalize()) != approved.sha256 {
        return Err(io::Error::other("provider full-file digest mismatch"));
    }
    Ok(())
}

#[cfg(windows)]
fn native_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut identity: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut identity) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if identity.dwFileAttributes & 0x400 != 0 {
        return Err(io::Error::other("provider object is a reparse point"));
    }
    Ok((
        u64::from(identity.dwVolumeSerialNumber),
        (u64::from(identity.nFileIndexHigh) << 32) | u64::from(identity.nFileIndexLow),
    ))
}

#[cfg(unix)]
fn native_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(any(unix, windows)))]
fn native_identity(_: &File) -> io::Result<(u64, u64)> {
    Err(io::Error::other("native provider identity is unavailable"))
}

#[cfg(all(test, windows, feature = "validation"))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fixture(path: PathBuf, bytes: &[u8]) -> ApprovedProviderFile {
        std::fs::write(&path, bytes).expect("fixture source");
        ApprovedProviderFile::from_retained(
            path.clone(),
            Sha256::digest(bytes).into(),
            strict_read(&path).expect("source lease"),
        )
    }

    #[test]
    fn approved_provider_namespace_command_and_consuming_close_are_one_owner() {
        let source = tempfile::tempdir().expect("source root");
        let approved = [
            fixture(
                source.path().join("tool.exe"),
                b"approved executable fixture",
            ),
            fixture(
                source.path().join("approval.json"),
                b"external approval fixture",
            ),
            fixture(source.path().join("profile.bin"), b"native profile fixture"),
        ];
        let deadline = Instant::now() + Duration::from_secs(5);
        let RegulatoryPseRuntimeAdmission::Available(runtime) = prepare_regulatory_pse_runtime(
            approved,
            Some(Vec::new()),
            deadline,
            &ExecutionCancellationToken::new(),
        )
        .expect("prepare namespace") else {
            panic!("explicit closure is present")
        };
        let path = runtime.owner.executable.clone();
        assert!(std::fs::write(
            path.parent().expect("capsule").join("unapproved.dll"),
            b"injected"
        )
        .is_err());
        assert!(std::fs::write(source.path().join("tool.exe"), b"replaced").is_err());
        let command = runtime
            .command(deadline, &ExecutionCancellationToken::new())
            .expect("typed command");
        assert_eq!(
            command.command.get_args().collect::<Vec<_>>(),
            [std::ffi::OsStr::new("--mondrian-regulatory-pse-v1")]
        );
        assert_eq!(Arc::strong_count(&runtime.owner), 2);
        drop(command);
        runtime.close(deadline).expect("consume runtime capsule");
        assert!(!path.exists());
        std::fs::write(
            source.path().join("tool.exe"),
            b"new bytes after explicit closure",
        )
        .expect("source lease released");
    }

    #[test]
    fn approved_provider_rejects_foreign_handle_even_with_identical_bytes() {
        let root = tempfile::tempdir().expect("root");
        let first = root.path().join("first.exe");
        let second = root.path().join("second.exe");
        std::fs::write(&first, b"identical").expect("first");
        std::fs::write(&second, b"identical").expect("second");
        let mut foreign = ApprovedProviderFile::from_retained(
            first,
            Sha256::digest(b"identical").into(),
            strict_read(&second).expect("foreign handle"),
        );
        assert!(pin_and_verify(
            &mut foreign,
            Instant::now() + Duration::from_secs(1),
            &ExecutionCancellationToken::new()
        )
        .expect_err("native identities must match")
        .to_string()
        .contains("same native object"));
    }

    #[test]
    fn provider_post_seal_failure_restores_and_consumes_exact_namespace() {
        let root = tempfile::tempdir().expect("source root");
        let mut owner = ProviderOwner {
            executable: root.path().join("provider.exe"),
            profile: root.path().join("profile.bin"),
            profile_index: 2,
            files: Mutex::new(vec![
                fixture(root.path().join("provider.exe"), b"executable"),
                fixture(root.path().join("approval.bin"), b"approval"),
                fixture(root.path().join("profile.bin"), b"profile"),
            ]),
            capsule: None,
        };
        let mut owned_path = None;
        let error = owner
            .prepare_capsule_after_seal(
                Instant::now() + Duration::from_secs(5),
                &ExecutionCancellationToken::new(),
                |path| {
                    owned_path = Some(path.to_path_buf());
                    assert!(std::fs::write(path.join("injected.dll"), b"injected").is_err());
                    Err(io::Error::other("observed post-seal rejection"))
                },
            )
            .expect_err("post-seal rejection");
        assert_eq!(error.to_string(), "observed post-seal rejection");
        assert!(!owned_path.expect("actual sealed directory").exists());
        owner.close(Instant::now() + Duration::from_secs(1)).expect("close sources");
    }

    #[test]
    fn provider_contended_file_owner_obeys_original_deadline() {
        let owner = ProviderOwner {
            executable: PathBuf::new(),
            profile: PathBuf::new(),
            profile_index: 2,
            files: Mutex::new(Vec::new()),
            capsule: None,
        };
        let guard = owner.files.lock();
        let began = Instant::now();
        let error = owner
            .validate(
                began + Duration::from_millis(20),
                &ExecutionCancellationToken::new(),
            )
            .expect_err("a held lease lock cannot reset the deadline");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(began.elapsed() < Duration::from_secs(1));
        drop(guard);
    }
}
