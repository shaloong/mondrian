//! Exact executable ownership for commercial FFmpeg qualification.
//!
//! Ordinary product execution resolves packaged tools through `ffmpeg_tools`.
//! A commercial endurance process instead prepares one private executable
//! snapshot for each tool, retains both the approved source objects and the
//! snapshots, and installs that pair once for every later CLI call.

#[cfg(windows)]
use std::collections::BTreeSet;
use std::fs::File;
#[cfg(windows)]
use std::fs::{self, OpenOptions};
#[cfg(windows)]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use mondrian_core::{ExecutionCancellationToken, MondrianError};
#[cfg(windows)]
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thiserror::Error;

#[cfg(windows)]
use crate::{
    run_supervised_command, SupervisedProcessError, SupervisedProcessPolicy,
    SupervisedStreamCapture,
};

#[cfg(windows)]
const MAXIMUM_EXECUTABLE_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(windows)]
const MAXIMUM_RUNTIME_FILE_COUNT: usize = 512;
#[cfg(windows)]
const MAXIMUM_RUNTIME_FILE_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(windows)]
const MAXIMUM_RUNTIME_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
#[cfg(windows)]
const MAXIMUM_TOOL_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
#[cfg(windows)]
const MAXIMUM_TOOL_STDERR_BYTES: usize = 1024 * 1024;
#[cfg(windows)]
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(windows)]
const CAPABILITY_REPORT_DOMAIN: &[u8] = b"mondrian/ffmpeg-capability-report/v1\0";

static PROCESS_TOOLCHAIN: OnceLock<Arc<PreparedFfmpegToolchain>> = OnceLock::new();

/// FFmpeg command-line executable role inside one exact toolchain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualifiedFfmpegToolKind {
    /// Encoder, decoder, filter, and mux/demux executable.
    Ffmpeg,
    /// Independent media-probe executable.
    Ffprobe,
}

impl QualifiedFfmpegToolKind {
    #[cfg(windows)]
    const fn label(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
        }
    }

    #[cfg(windows)]
    fn snapshot_file_name(self) -> String {
        format!("{}{}", self.label(), std::env::consts::EXE_SUFFIX)
    }

    #[cfg(any(windows, test))]
    const fn capability_arguments(self) -> &'static [&'static [&'static str]] {
        const COMMON: &[&[&str]] = &[
            &["-hide_banner", "-buildconf"],
            &["-hide_banner", "-formats"],
            &["-hide_banner", "-codecs"],
            &["-hide_banner", "-decoders"],
            &["-hide_banner", "-pix_fmts"],
            &["-hide_banner", "-sample_fmts"],
            &["-hide_banner", "-layouts"],
            &["-hide_banner", "-protocols"],
        ];
        const FFMPEG: &[&[&str]] = &[
            &["-hide_banner", "-buildconf"],
            &["-hide_banner", "-formats"],
            &["-hide_banner", "-codecs"],
            &["-hide_banner", "-encoders"],
            &["-hide_banner", "-decoders"],
            &["-hide_banner", "-filters"],
            &["-hide_banner", "-pix_fmts"],
            &["-hide_banner", "-sample_fmts"],
            &["-hide_banner", "-layouts"],
            &["-hide_banner", "-hwaccels"],
            &["-hide_banner", "-protocols"],
        ];
        match self {
            Self::Ffmpeg => FFMPEG,
            Self::Ffprobe => COMMON,
        }
    }
}

/// Externally approved identity for one executable and its fixed probes.
#[derive(Debug, Clone, Copy)]
pub struct QualifiedFfmpegToolExpectation<'a> {
    /// Canonical direct executable path.
    pub executable_path: &'a Path,
    /// Lowercase SHA-256 of the exact executable bytes.
    pub executable_sha256: &'a str,
    /// Lowercase SHA-256 of raw stdout from the fixed `-version` invocation.
    pub version_output_sha256: &'a str,
    /// Lowercase SHA-256 of the version-1 framed capability report.
    pub capability_report_sha256: &'a str,
}

/// Externally approved identity for one packaged FFmpeg runtime DLL.
#[derive(Debug, Clone, Copy)]
pub struct QualifiedFfmpegRuntimeFileExpectation<'a> {
    /// Canonical direct DLL path beside both command-line tools.
    pub path: &'a Path,
    /// Lowercase SHA-256 of the exact DLL bytes.
    pub sha256: &'a str,
}

/// Immutable public identity returned after one runtime DLL is snapshotted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedFfmpegRuntimeFileReceipt {
    /// Canonical approved source path.
    pub source_path: PathBuf,
    /// SHA-256 observed from both source and private snapshot objects.
    pub sha256: String,
    /// Exact file length admitted before copying.
    pub byte_length: u64,
}

/// Immutable public identity returned only after one tool has been prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedFfmpegToolReceipt {
    /// Tool role represented by this receipt.
    pub kind: QualifiedFfmpegToolKind,
    /// Canonical approved source path.
    pub source_path: PathBuf,
    /// SHA-256 observed from the retained source object.
    pub executable_sha256: String,
    /// SHA-256 observed from the retained private executable snapshot.
    pub snapshot_sha256: String,
    /// SHA-256 of exact raw `-version` stdout from the snapshot.
    pub version_output_sha256: String,
    /// SHA-256 of the fixed framed capability report from the snapshot.
    pub capability_report_sha256: String,
}

#[derive(Debug)]
struct PreparedFfmpegTool {
    receipt: QualifiedFfmpegToolReceipt,
    snapshot_path: PathBuf,
    snapshot_directory: PathBuf,
    _source_lease: File,
    _snapshot_lease: File,
}

impl PreparedFfmpegTool {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.snapshot_path);
        command.current_dir(&self.snapshot_directory);
        command.env("PATH", &self.snapshot_directory);
        command
    }
}

#[derive(Debug)]
struct PreparedFfmpegRuntimeFile {
    receipt: QualifiedFfmpegRuntimeFileReceipt,
    _source_lease: File,
    _snapshot_lease: File,
}

/// Process-retained exact FFmpeg/FFprobe executable pair.
///
/// Construction copies each verified source object into a private unique
/// directory, verifies and locks the copied object, then runs bounded fixed
/// probes through that snapshot. Call [`install_process_ffmpeg_toolchain`] once
/// before constructing qualification phase owners so all ordinary media and
/// Export CLI seams resolve to these exact snapshots.
#[derive(Debug)]
pub struct PreparedFfmpegToolchain {
    ffmpeg: PreparedFfmpegTool,
    ffprobe: PreparedFfmpegTool,
    runtime_files: Vec<PreparedFfmpegRuntimeFile>,
    _snapshot_directory_lease: File,
    _snapshot_directory: TempDir,
    #[cfg(windows)]
    namespace_poisoned: AtomicBool,
}

impl PreparedFfmpegToolchain {
    /// Prepare an exact Windows executable pair against approved identities.
    pub fn prepare(
        ffmpeg: QualifiedFfmpegToolExpectation<'_>,
        ffprobe: QualifiedFfmpegToolExpectation<'_>,
        runtime_files: &[QualifiedFfmpegRuntimeFileExpectation<'_>],
    ) -> Result<Arc<Self>, QualifiedFfmpegToolchainError> {
        #[cfg(not(windows))]
        {
            let _ = (ffmpeg, ffprobe, runtime_files);
            return Err(QualifiedFfmpegToolchainError::UnsupportedPlatform);
        }

        #[cfg(windows)]
        {
            let snapshot_directory = tempfile::Builder::new()
                .prefix("mondrian-qualified-ffmpeg-")
                .tempdir()
                .map_err(QualifiedFfmpegToolchainError::SnapshotDirectory)?;
            let snapshot_directory_lease = open_direct_read_directory(snapshot_directory.path())
                .map_err(QualifiedFfmpegToolchainError::SnapshotDirectory)?;
            let prepared_runtime_files = prepare_runtime_files(
                ffmpeg.executable_path,
                ffprobe.executable_path,
                runtime_files,
                snapshot_directory.path(),
            )?;
            verify_linked_runtime_identity(&prepared_runtime_files)?;
            let prepared_ffmpeg = prepare_tool(
                QualifiedFfmpegToolKind::Ffmpeg,
                ffmpeg,
                snapshot_directory.path(),
            )?;
            let prepared_ffprobe = prepare_tool(
                QualifiedFfmpegToolKind::Ffprobe,
                ffprobe,
                snapshot_directory.path(),
            )?;
            verify_command_semantics(&prepared_ffmpeg)?;
            Ok(Arc::new(Self {
                ffmpeg: prepared_ffmpeg,
                ffprobe: prepared_ffprobe,
                runtime_files: prepared_runtime_files,
                _snapshot_directory_lease: snapshot_directory_lease,
                _snapshot_directory: snapshot_directory,
                namespace_poisoned: AtomicBool::new(false),
            }))
        }
    }

    /// Exact prepared FFmpeg identity.
    pub const fn ffmpeg_receipt(&self) -> &QualifiedFfmpegToolReceipt {
        &self.ffmpeg.receipt
    }

    /// Exact prepared ffprobe identity.
    pub const fn ffprobe_receipt(&self) -> &QualifiedFfmpegToolReceipt {
        &self.ffprobe.receipt
    }

    /// Complete ordered packaged runtime-DLL closure retained by the process.
    pub fn runtime_file_receipts(
        &self,
    ) -> impl ExactSizeIterator<Item = &QualifiedFfmpegRuntimeFileReceipt> {
        self.runtime_files.iter().map(|file| &file.receipt)
    }

    /// Revalidate the retained source identities and exact capsule namespace.
    pub fn validate_current(&self) -> Result<(), QualifiedFfmpegToolchainError> {
        #[cfg(not(windows))]
        {
            return Err(QualifiedFfmpegToolchainError::UnsupportedPlatform);
        }
        #[cfg(windows)]
        {
            if self.namespace_poisoned.load(Ordering::Acquire) || !capsule_namespace_matches(self) {
                self.namespace_poisoned.store(true, Ordering::Release);
                return Err(QualifiedFfmpegToolchainError::CapsuleNamespaceChanged);
            }
            verify_linked_runtime_identity(&self.runtime_files)
        }
    }

    pub(crate) fn ffmpeg_command(&self) -> Command {
        if self.validate_current().is_ok() {
            self.ffmpeg.command()
        } else {
            Command::new(self._snapshot_directory.path().join("invalid-ffmpeg-identity"))
        }
    }

    pub(crate) fn ffprobe_command(&self) -> Command {
        if self.validate_current().is_ok() {
            self.ffprobe.command()
        } else {
            Command::new(self._snapshot_directory.path().join("invalid-ffprobe-identity"))
        }
    }

    fn same_identity(&self, other: &Self) -> bool {
        self.ffmpeg.receipt == other.ffmpeg.receipt
            && self.ffprobe.receipt == other.ffprobe.receipt
            && self.runtime_file_receipts().eq(other.runtime_file_receipts())
    }
}

pub(crate) fn verify_installed_process_toolchain() -> Result<bool, QualifiedFfmpegToolchainError> {
    let Some(toolchain) = PROCESS_TOOLCHAIN.get() else {
        return Ok(false);
    };
    toolchain.validate_current()?;
    #[cfg(windows)]
    verify_command_semantics(&toolchain.ffmpeg)?;
    Ok(true)
}

/// Install the only qualification FFmpeg toolchain admitted in this process.
///
/// Reinstalling the same identity is idempotent. A different pair is rejected
/// because already-created owners could otherwise observe two executable
/// identities during one campaign.
pub fn install_process_ffmpeg_toolchain(
    toolchain: Arc<PreparedFfmpegToolchain>,
) -> Result<Arc<PreparedFfmpegToolchain>, QualifiedFfmpegToolchainError> {
    toolchain.validate_current()?;
    if let Some(installed) = PROCESS_TOOLCHAIN.get() {
        installed.validate_current()?;
        return if installed.same_identity(&toolchain) {
            Ok(Arc::clone(installed))
        } else {
            Err(QualifiedFfmpegToolchainError::ProcessIdentityConflict)
        };
    }
    match PROCESS_TOOLCHAIN.set(toolchain) {
        Ok(()) => {
            let installed = PROCESS_TOOLCHAIN
                .get()
                .ok_or(QualifiedFfmpegToolchainError::ProcessIdentityConflict)?;
            installed.validate_current()?;
            Ok(Arc::clone(installed))
        }
        Err(candidate) => match PROCESS_TOOLCHAIN.get() {
            Some(installed) if installed.same_identity(&candidate) => {
                installed.validate_current()?;
                Ok(Arc::clone(installed))
            }
            Some(_) | None => Err(QualifiedFfmpegToolchainError::ProcessIdentityConflict),
        },
    }
}

pub(crate) fn process_ffmpeg_command() -> Option<Command> {
    PROCESS_TOOLCHAIN.get().map(|toolchain| toolchain.ffmpeg_command())
}

pub(crate) fn process_ffprobe_command() -> Option<Command> {
    PROCESS_TOOLCHAIN.get().map(|toolchain| toolchain.ffprobe_command())
}

#[cfg(windows)]
fn capsule_namespace_matches(toolchain: &PreparedFfmpegToolchain) -> bool {
    let expected = std::iter::once(toolchain.ffmpeg.snapshot_path.file_name())
        .chain(std::iter::once(toolchain.ffprobe.snapshot_path.file_name()))
        .chain(toolchain.runtime_files.iter().map(|file| file.receipt.source_path.file_name()))
        .flatten()
        .map(std::ffi::OsStr::to_os_string)
        .collect::<BTreeSet<_>>();
    capsule_directory_matches(toolchain._snapshot_directory.path(), &expected)
}

#[cfg(windows)]
fn capsule_directory_matches(directory: &Path, expected: &BTreeSet<std::ffi::OsString>) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let mut actual = BTreeSet::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            return false;
        };
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_file()
            || metadata_has_reparse_point(&metadata)
            || !actual.insert(entry.file_name())
        {
            return false;
        }
    }
    &actual == expected
}

#[cfg(windows)]
fn prepare_runtime_files(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    expectations: &[QualifiedFfmpegRuntimeFileExpectation<'_>],
    snapshot_directory: &Path,
) -> Result<Vec<PreparedFfmpegRuntimeFile>, QualifiedFfmpegToolchainError> {
    if expectations.is_empty() || expectations.len() > MAXIMUM_RUNTIME_FILE_COUNT {
        return Err(QualifiedFfmpegToolchainError::InvalidRuntimeClosure);
    }
    let source_directory = ffmpeg_path
        .parent()
        .filter(|directory| Some(*directory) == ffprobe_path.parent())
        .ok_or(QualifiedFfmpegToolchainError::InvalidRuntimeClosure)?;
    let declared_paths = expectations
        .iter()
        .map(|expectation| expectation.path.to_path_buf())
        .collect::<Vec<_>>();
    if declared_paths.windows(2).any(|pair| pair[0] >= pair[1])
        || declared_paths
            .iter()
            .any(|path| path.parent() != Some(source_directory) || !is_dll(path))
    {
        return Err(QualifiedFfmpegToolchainError::InvalidRuntimeClosure);
    }
    let declared_set = declared_paths.iter().cloned().collect::<BTreeSet<_>>();
    if declared_set.len() != declared_paths.len() {
        return Err(QualifiedFfmpegToolchainError::InvalidRuntimeClosure);
    }
    let discovered_set = discover_direct_runtime_files(source_directory)?;
    if declared_set != discovered_set {
        return Err(QualifiedFfmpegToolchainError::RuntimeClosureMismatch {
            declared: declared_set.len(),
            discovered: discovered_set.len(),
        });
    }

    let mut prepared = Vec::with_capacity(expectations.len());
    let mut aggregate_bytes = 0_u64;
    for expectation in expectations {
        validate_sha256(
            expectation.sha256,
            QualifiedFfmpegToolKind::Ffmpeg,
            "runtime_file_sha256",
        )?;
        let canonical_path = ordinary_canonical_path(expectation.path).map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeSourceOpen {
                path: expectation.path.to_path_buf(),
                source,
            }
        })?;
        if canonical_path != expectation.path {
            return Err(QualifiedFfmpegToolchainError::RuntimePathNotCanonical {
                path: expectation.path.to_path_buf(),
            });
        }
        let mut source = open_direct_read_file(&canonical_path).map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeSourceOpen {
                path: canonical_path.clone(),
                source,
            }
        })?;
        let length = source
            .metadata()
            .map_err(|source| QualifiedFfmpegToolchainError::RuntimeSourceOpen {
                path: canonical_path.clone(),
                source,
            })?
            .len();
        if length == 0 || length > MAXIMUM_RUNTIME_FILE_BYTES {
            return Err(QualifiedFfmpegToolchainError::InvalidRuntimeFileSize {
                path: canonical_path,
                actual: length,
            });
        }
        aggregate_bytes = aggregate_bytes
            .checked_add(length)
            .filter(|total| *total <= MAXIMUM_RUNTIME_TOTAL_BYTES)
            .ok_or(QualifiedFfmpegToolchainError::RuntimeClosureTooLarge)?;
        let file_name = canonical_path
            .file_name()
            .ok_or(QualifiedFfmpegToolchainError::InvalidRuntimeClosure)?;
        let snapshot_path = snapshot_directory.join(file_name);
        let mut snapshot = create_direct_exclusive_file(&snapshot_path).map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeSnapshotWrite {
                path: snapshot_path.clone(),
                source,
            }
        })?;
        let observed_sha = copy_and_hash(&mut source, &mut snapshot, length).map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeSnapshotWrite {
                path: snapshot_path.clone(),
                source,
            }
        })?;
        if observed_sha != expectation.sha256 {
            return Err(QualifiedFfmpegToolchainError::RuntimeHashMismatch {
                path: canonical_path,
                expected: expectation.sha256.to_owned(),
                actual: observed_sha,
            });
        }
        snapshot.sync_all().map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeSnapshotWrite {
                path: snapshot_path.clone(),
                source,
            }
        })?;
        drop(snapshot);
        let mut snapshot_lease = open_direct_read_file(&snapshot_path).map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeSnapshotOpen {
                path: snapshot_path.clone(),
                source,
            }
        })?;
        let snapshot_sha = hash_bounded_file(&mut snapshot_lease, length).map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeSnapshotOpen {
                path: snapshot_path.clone(),
                source,
            }
        })?;
        if snapshot_sha != expectation.sha256 {
            return Err(QualifiedFfmpegToolchainError::RuntimeHashMismatch {
                path: snapshot_path,
                expected: expectation.sha256.to_owned(),
                actual: snapshot_sha,
            });
        }
        prepared.push(PreparedFfmpegRuntimeFile {
            receipt: QualifiedFfmpegRuntimeFileReceipt {
                source_path: canonical_path,
                sha256: expectation.sha256.to_owned(),
                byte_length: length,
            },
            _source_lease: source,
            _snapshot_lease: snapshot_lease,
        });
    }
    Ok(prepared)
}

#[cfg(windows)]
fn discover_direct_runtime_files(
    source_directory: &Path,
) -> Result<BTreeSet<PathBuf>, QualifiedFfmpegToolchainError> {
    let mut files = BTreeSet::new();
    let entries = fs::read_dir(source_directory).map_err(|source| {
        QualifiedFfmpegToolchainError::RuntimeDirectoryRead {
            path: source_directory.to_path_buf(),
            source,
        }
    })?;
    for entry in entries {
        let entry =
            entry.map_err(
                |source| QualifiedFfmpegToolchainError::RuntimeDirectoryRead {
                    path: source_directory.to_path_buf(),
                    source,
                },
            )?;
        let path = entry.path();
        if !is_dll(&path) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeDirectoryRead {
                path: source_directory.to_path_buf(),
                source,
            }
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_file()
            || metadata_has_reparse_point(&metadata)
        {
            return Err(QualifiedFfmpegToolchainError::InvalidRuntimeClosure);
        }
        files.insert(ordinary_canonical_path(&path).map_err(|source| {
            QualifiedFfmpegToolchainError::RuntimeDirectoryRead {
                path: source_directory.to_path_buf(),
                source,
            }
        })?);
    }
    Ok(files)
}

#[cfg(windows)]
fn is_dll(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
}

#[cfg(windows)]
fn verify_linked_runtime_identity(
    runtime_files: &[PreparedFfmpegRuntimeFile],
) -> Result<(), QualifiedFfmpegToolchainError> {
    crate::ffmpeg_runtime::ensure_ffmpeg_initialized(Path::new("<qualified-ffmpeg-runtime>"))
        .map_err(|error| QualifiedFfmpegToolchainError::LinkedRuntime(error.to_string()))?;
    if runtime_files.is_empty() {
        return Err(QualifiedFfmpegToolchainError::InvalidRuntimeClosure);
    }
    let mut verified_components = BTreeSet::new();
    for runtime_file in runtime_files {
        let Some(file_name) = runtime_file.receipt.source_path.file_name() else {
            return Err(QualifiedFfmpegToolchainError::InvalidRuntimeClosure);
        };
        let lower_name = file_name.to_string_lossy().to_ascii_lowercase();
        let component = linked_ffmpeg_component(&lower_name);
        let loaded_path = loaded_module_path(file_name)?;
        if component.is_some() && loaded_path.is_none() {
            return Err(QualifiedFfmpegToolchainError::LinkedRuntime(format!(
                "required loaded module {lower_name} was not found"
            )));
        }
        if loaded_path
            .as_ref()
            .is_some_and(|path| path != &runtime_file.receipt.source_path)
        {
            return Err(QualifiedFfmpegToolchainError::LinkedRuntime(format!(
                "loaded module {} differs from approved source {}",
                loaded_path
                    .as_ref()
                    .map_or_else(|| "<missing>".to_owned(), |path| path.display().to_string()),
                runtime_file.receipt.source_path.display()
            )));
        }
        if let Some(component) = component {
            verified_components.insert(component);
        }
    }
    let required = [
        "avcodec",
        "avformat",
        "avutil",
        "avfilter",
        "swscale",
        "swresample",
    ];
    if required.iter().any(|component| !verified_components.contains(component)) {
        return Err(QualifiedFfmpegToolchainError::LinkedRuntime(
            "approved closure does not bind every linked FFmpeg component".to_owned(),
        ));
    }
    let approved_paths = runtime_files
        .iter()
        .map(|file| file.receipt.source_path.clone())
        .collect::<BTreeSet<_>>();
    let system_directories = windows_system_directories()?;
    for loaded_path in loaded_process_module_paths()? {
        if is_dll(&loaded_path)
            && !module_path_is_admitted(&loaded_path, &approved_paths, &system_directories)
        {
            return Err(QualifiedFfmpegToolchainError::LinkedRuntime(format!(
                "loaded non-system module {} is outside the approved runtime closure",
                loaded_path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn module_path_is_admitted(
    loaded_path: &Path,
    approved_paths: &BTreeSet<PathBuf>,
    system_directories: &[PathBuf],
) -> bool {
    approved_paths.contains(loaded_path)
        || system_directories.iter().any(|directory| loaded_path.starts_with(directory))
}

#[cfg(windows)]
fn loaded_module_path(
    file_name: &std::ffi::OsStr,
) -> Result<Option<PathBuf>, QualifiedFfmpegToolchainError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};

    let mut wide_name = file_name.encode_wide().collect::<Vec<_>>();
    wide_name.push(0);
    let module = unsafe { GetModuleHandleW(wide_name.as_ptr()) };
    if module.is_null() {
        return Ok(None);
    }
    let mut path_buffer = vec![0_u16; 32_768];
    let length = unsafe {
        GetModuleFileNameW(
            module,
            path_buffer.as_mut_ptr(),
            u32::try_from(path_buffer.len()).unwrap_or(u32::MAX),
        )
    };
    canonical_module_path(path_buffer, length).map(Some)
}

#[cfg(windows)]
fn loaded_process_module_paths() -> Result<Vec<PathBuf>, QualifiedFfmpegToolchainError> {
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::ProcessStatus::{
        K32EnumProcessModules, K32GetModuleFileNameExW,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    const INITIAL_MODULE_COUNT: usize = 256;
    const MAXIMUM_MODULE_COUNT: usize = 4096;

    let process = unsafe { GetCurrentProcess() };
    let module_bytes = std::mem::size_of::<HMODULE>();
    let mut modules = vec![std::ptr::null_mut(); INITIAL_MODULE_COUNT];
    loop {
        let buffer_bytes = modules.len().checked_mul(module_bytes).ok_or_else(|| {
            QualifiedFfmpegToolchainError::LinkedRuntime(
                "loaded process-module buffer size overflowed".to_owned(),
            )
        })?;
        let buffer_bytes_u32 = u32::try_from(buffer_bytes).map_err(|_| {
            QualifiedFfmpegToolchainError::LinkedRuntime(
                "loaded process-module buffer exceeded the Windows API bound".to_owned(),
            )
        })?;
        let mut needed = 0_u32;
        if unsafe {
            K32EnumProcessModules(process, modules.as_mut_ptr(), buffer_bytes_u32, &mut needed)
        } == 0
        {
            return Err(QualifiedFfmpegToolchainError::LinkedRuntime(
                "could not enumerate loaded process modules".to_owned(),
            ));
        }
        let needed = usize::try_from(needed).map_err(|_| {
            QualifiedFfmpegToolchainError::LinkedRuntime(
                "loaded process-module byte count could not be represented".to_owned(),
            )
        })?;
        if needed <= buffer_bytes {
            if needed == 0 || needed % module_bytes != 0 {
                return Err(QualifiedFfmpegToolchainError::LinkedRuntime(
                    "loaded process-module byte count was invalid".to_owned(),
                ));
            }
            modules.truncate(needed / module_bytes);
            break;
        }
        let required_count = needed
            .checked_add(module_bytes - 1)
            .map(|bytes| bytes / module_bytes)
            .ok_or_else(|| {
                QualifiedFfmpegToolchainError::LinkedRuntime(
                    "loaded process-module count overflowed".to_owned(),
                )
            })?;
        if required_count > MAXIMUM_MODULE_COUNT {
            return Err(QualifiedFfmpegToolchainError::LinkedRuntime(format!(
                "loaded process-module count {required_count} exceeds the fixed bound"
            )));
        }
        modules.resize(required_count, std::ptr::null_mut());
    }
    let mut paths = Vec::with_capacity(modules.len());
    for module in modules {
        let mut path_buffer = vec![0_u16; 32_768];
        let length = unsafe {
            K32GetModuleFileNameExW(
                process,
                module,
                path_buffer.as_mut_ptr(),
                u32::try_from(path_buffer.len()).unwrap_or(u32::MAX),
            )
        };
        paths.push(canonical_module_path(path_buffer, length)?);
    }
    Ok(paths)
}

#[cfg(windows)]
fn canonical_module_path(
    mut path_buffer: Vec<u16>,
    length: u32,
) -> Result<PathBuf, QualifiedFfmpegToolchainError> {
    use std::os::windows::ffi::OsStringExt;

    if length == 0 || usize::try_from(length).ok() == Some(path_buffer.len()) {
        return Err(QualifiedFfmpegToolchainError::LinkedRuntime(
            "could not resolve a loaded module path".to_owned(),
        ));
    }
    path_buffer.truncate(usize::try_from(length).unwrap_or(0));
    ordinary_canonical_path(&PathBuf::from(std::ffi::OsString::from_wide(&path_buffer)))
        .map_err(|error| QualifiedFfmpegToolchainError::LinkedRuntime(error.to_string()))
}

#[cfg(windows)]
fn windows_system_directories() -> Result<Vec<PathBuf>, QualifiedFfmpegToolchainError> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::{
        GetSystemDirectoryW, GetWindowsDirectoryW,
    };

    fn resolve(
        get_directory: unsafe extern "system" fn(*mut u16, u32) -> u32,
        description: &str,
    ) -> Result<PathBuf, QualifiedFfmpegToolchainError> {
        let mut buffer = vec![0_u16; 32_768];
        let length = unsafe {
            get_directory(
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).unwrap_or(u32::MAX),
            )
        };
        if length == 0 || usize::try_from(length).ok().is_none_or(|length| length >= buffer.len()) {
            return Err(QualifiedFfmpegToolchainError::LinkedRuntime(format!(
                "could not resolve the Windows {description} directory"
            )));
        }
        buffer.truncate(usize::try_from(length).unwrap_or(0));
        ordinary_canonical_path(&PathBuf::from(std::ffi::OsString::from_wide(&buffer)))
            .map_err(|error| QualifiedFfmpegToolchainError::LinkedRuntime(error.to_string()))
    }

    let windows = resolve(GetWindowsDirectoryW, "root")?;
    let system = resolve(GetSystemDirectoryW, "system")?;
    let mut directories = vec![system];
    for relative in ["SysWOW64", "WinSxS"] {
        let candidate = windows.join(relative);
        if candidate.is_dir() {
            directories.push(ordinary_canonical_path(&candidate).map_err(|error| {
                QualifiedFfmpegToolchainError::LinkedRuntime(error.to_string())
            })?);
        }
    }
    Ok(directories)
}

#[cfg(windows)]
fn linked_ffmpeg_component(file_name: &str) -> Option<&'static str> {
    [
        "avcodec",
        "avformat",
        "avutil",
        "avfilter",
        "swscale",
        "swresample",
    ]
    .into_iter()
    .find(|component| {
        file_name == format!("{component}.dll") || file_name.starts_with(&format!("{component}-"))
    })
}

#[cfg(windows)]
fn prepare_tool(
    kind: QualifiedFfmpegToolKind,
    expectation: QualifiedFfmpegToolExpectation<'_>,
    snapshot_directory: &Path,
) -> Result<PreparedFfmpegTool, QualifiedFfmpegToolchainError> {
    validate_sha256(expectation.executable_sha256, kind, "executable_sha256")?;
    validate_sha256(
        expectation.version_output_sha256,
        kind,
        "version_output_sha256",
    )?;
    validate_sha256(
        expectation.capability_report_sha256,
        kind,
        "capability_report_sha256",
    )?;
    let canonical_path = ordinary_canonical_path(expectation.executable_path)
        .map_err(|source| QualifiedFfmpegToolchainError::SourceOpen { kind, source })?;
    if canonical_path != expectation.executable_path {
        return Err(QualifiedFfmpegToolchainError::SourcePathNotCanonical { kind });
    }
    let mut source = open_direct_read_file(&canonical_path)
        .map_err(|source| QualifiedFfmpegToolchainError::SourceOpen { kind, source })?;
    let source_metadata = source
        .metadata()
        .map_err(|source| QualifiedFfmpegToolchainError::SourceOpen { kind, source })?;
    if !source_metadata.is_file()
        || source_metadata.len() == 0
        || source_metadata.len() > MAXIMUM_EXECUTABLE_BYTES
    {
        return Err(QualifiedFfmpegToolchainError::InvalidExecutableSize {
            kind,
            actual: source_metadata.len(),
        });
    }

    let snapshot_path = snapshot_directory.join(kind.snapshot_file_name());
    let mut snapshot = create_direct_exclusive_file(&snapshot_path)
        .map_err(|source| QualifiedFfmpegToolchainError::SnapshotWrite { kind, source })?;
    let observed_source_sha = copy_and_hash(&mut source, &mut snapshot, source_metadata.len())
        .map_err(|source| QualifiedFfmpegToolchainError::SnapshotWrite { kind, source })?;
    if observed_source_sha != expectation.executable_sha256 {
        return Err(QualifiedFfmpegToolchainError::ExecutableHashMismatch {
            kind,
            expected: expectation.executable_sha256.to_owned(),
            actual: observed_source_sha,
        });
    }
    snapshot
        .sync_all()
        .map_err(|source| QualifiedFfmpegToolchainError::SnapshotWrite { kind, source })?;
    drop(snapshot);
    let mut snapshot_lease = open_direct_read_file(&snapshot_path)
        .map_err(|source| QualifiedFfmpegToolchainError::SnapshotOpen { kind, source })?;
    let snapshot_sha = hash_bounded_file(&mut snapshot_lease, source_metadata.len())
        .map_err(|source| QualifiedFfmpegToolchainError::SnapshotOpen { kind, source })?;
    if snapshot_sha != expectation.executable_sha256 {
        return Err(QualifiedFfmpegToolchainError::SnapshotHashMismatch {
            kind,
            expected: expectation.executable_sha256.to_owned(),
            actual: snapshot_sha,
        });
    }

    let version_output = run_tool(&snapshot_path, kind, &["-version"])?;
    let version_output_sha = lower_sha256(&version_output);
    if version_output_sha != expectation.version_output_sha256 {
        return Err(QualifiedFfmpegToolchainError::VersionOutputMismatch {
            kind,
            expected: expectation.version_output_sha256.to_owned(),
            actual: version_output_sha,
        });
    }

    let capability_report = capture_capability_report(&snapshot_path, kind)?;
    let capability_report_sha = lower_sha256(&capability_report);
    if capability_report_sha != expectation.capability_report_sha256 {
        return Err(QualifiedFfmpegToolchainError::CapabilityReportMismatch {
            kind,
            expected: expectation.capability_report_sha256.to_owned(),
            actual: capability_report_sha,
        });
    }

    Ok(PreparedFfmpegTool {
        receipt: QualifiedFfmpegToolReceipt {
            kind,
            source_path: canonical_path,
            executable_sha256: expectation.executable_sha256.to_owned(),
            snapshot_sha256: snapshot_sha,
            version_output_sha256: version_output_sha,
            capability_report_sha256: capability_report_sha,
        },
        snapshot_path,
        snapshot_directory: snapshot_directory.to_path_buf(),
        _source_lease: source,
        _snapshot_lease: snapshot_lease,
    })
}

#[cfg(windows)]
fn verify_command_semantics(
    ffmpeg: &PreparedFfmpegTool,
) -> Result<(), QualifiedFfmpegToolchainError> {
    let encoders = run_tool(
        &ffmpeg.snapshot_path,
        QualifiedFfmpegToolKind::Ffmpeg,
        &["-hide_banner", "-encoders"],
    )?;
    let filters = run_tool(
        &ffmpeg.snapshot_path,
        QualifiedFfmpegToolKind::Ffmpeg,
        &["-hide_banner", "-filters"],
    )?;
    let muxers = run_tool(
        &ffmpeg.snapshot_path,
        QualifiedFfmpegToolKind::Ffmpeg,
        &["-hide_banner", "-muxers"],
    )?;
    let build_configuration = run_tool(
        &ffmpeg.snapshot_path,
        QualifiedFfmpegToolKind::Ffmpeg,
        &["-hide_banner", "-buildconf"],
    )?;
    if String::from_utf8_lossy(&build_configuration).contains("--enable-nonfree") {
        return Err(QualifiedFfmpegToolchainError::CommandSemantic(
            "qualified FFmpeg was built with --enable-nonfree".to_owned(),
        ));
    }
    crate::ffmpeg_runtime::verify_required_decoders(Path::new("<qualified-linked-runtime>"))
        .map_err(|error| QualifiedFfmpegToolchainError::CommandSemantic(error.to_string()))?;
    crate::ffmpeg_runtime::verify_qualified_command_capabilities(
        Path::new("<qualified-command-runtime>"),
        &String::from_utf8_lossy(&encoders),
        &String::from_utf8_lossy(&filters),
        &String::from_utf8_lossy(&muxers),
        |encoder| {
            run_tool(
                &ffmpeg.snapshot_path,
                QualifiedFfmpegToolKind::Ffmpeg,
                &["-hide_banner", "-h", &format!("encoder={encoder}")],
            )
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|error| MondrianError::MediaOpen {
                path: ffmpeg.snapshot_path.display().to_string(),
                reason: error.to_string(),
            })
        },
    )
    .map_err(|error| QualifiedFfmpegToolchainError::CommandSemantic(error.to_string()))
}

#[cfg(windows)]
fn capture_capability_report(
    executable: &Path,
    kind: QualifiedFfmpegToolKind,
) -> Result<Vec<u8>, QualifiedFfmpegToolchainError> {
    let mut report = Vec::with_capacity(1024 * 1024);
    report.extend_from_slice(CAPABILITY_REPORT_DOMAIN);
    for arguments in kind.capability_arguments() {
        append_framed(&mut report, arguments.join("\0").as_bytes(), kind)?;
        let output = run_tool(executable, kind, arguments)?;
        append_framed(&mut report, &output, kind)?;
        if report.len() > MAXIMUM_TOOL_OUTPUT_BYTES {
            return Err(QualifiedFfmpegToolchainError::CapabilityReportTooLarge { kind });
        }
    }
    Ok(report)
}

#[cfg(windows)]
fn append_framed(
    report: &mut Vec<u8>,
    bytes: &[u8],
    kind: QualifiedFfmpegToolKind,
) -> Result<(), QualifiedFfmpegToolchainError> {
    let length = u64::try_from(bytes.len())
        .map_err(|_| QualifiedFfmpegToolchainError::CapabilityReportTooLarge { kind })?;
    report.extend_from_slice(&length.to_le_bytes());
    report.extend_from_slice(bytes);
    Ok(())
}

#[cfg(windows)]
fn run_tool(
    executable: &Path,
    kind: QualifiedFfmpegToolKind,
    arguments: &[&str],
) -> Result<Vec<u8>, QualifiedFfmpegToolchainError> {
    let deadline = Instant::now()
        .checked_add(TOOL_PROBE_TIMEOUT)
        .ok_or(QualifiedFfmpegToolchainError::ProbeDeadlineOverflow { kind })?;
    let mut command = Command::new(executable);
    command.args(arguments);
    if let Some(snapshot_directory) = executable.parent() {
        command.current_dir(snapshot_directory);
        command.env("PATH", snapshot_directory);
    }
    let policy = SupervisedProcessPolicy {
        pipe_stdin: false,
        stdout: SupervisedStreamCapture::Head {
            limit_bytes: MAXIMUM_TOOL_OUTPUT_BYTES,
            reject_excess: true,
        },
        stderr: SupervisedStreamCapture::Head {
            limit_bytes: MAXIMUM_TOOL_STDERR_BYTES,
            reject_excess: true,
        },
        deadline: Some(deadline),
        ..SupervisedProcessPolicy::default()
    };
    let output = run_supervised_command(
        &mut command,
        None,
        policy,
        &ExecutionCancellationToken::new(),
    )
    .map_err(|source| QualifiedFfmpegToolchainError::ProbeProcess { kind, source })?;
    if !output.status.success() {
        return Err(QualifiedFfmpegToolchainError::ProbeFailed {
            kind,
            arguments: arguments.join(" "),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output.stdout)
}

#[cfg(windows)]
fn copy_and_hash(
    source: &mut File,
    destination: &mut File,
    expected: u64,
) -> std::io::Result<String> {
    source.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut observed = 0_u64;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("executable byte count overflowed"))?;
        if observed > expected {
            return Err(std::io::Error::other("executable grew during snapshot"));
        }
        destination.write_all(&buffer[..read])?;
        digest.update(&buffer[..read]);
    }
    if observed != expected {
        return Err(std::io::Error::other(
            "executable length changed during snapshot",
        ));
    }
    source.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(windows)]
fn hash_bounded_file(file: &mut File, expected: u64) -> std::io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut observed = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("snapshot byte count overflowed"))?;
        if observed > expected {
            return Err(std::io::Error::other("snapshot exceeds expected length"));
        }
        digest.update(&buffer[..read]);
    }
    if observed != expected {
        return Err(std::io::Error::other("snapshot length differs from source"));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(windows)]
fn open_direct_read_file(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata_has_reparse_point(&metadata) {
        return Err(std::io::Error::other("path is not a direct regular file"));
    }
    Ok(file)
}

#[cfg(windows)]
fn create_direct_exclusive_file(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(windows)]
fn open_direct_read_directory(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    let directory = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata_has_reparse_point(&metadata) {
        return Err(std::io::Error::other("path is not a direct directory"));
    }
    Ok(directory)
}

#[cfg(windows)]
fn metadata_has_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(windows)]
fn ordinary_canonical_path(path: &Path) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    const VERBATIM_PREFIX: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_UNC_PREFIX: &[u16] = &[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];
    let canonical = fs::canonicalize(path)?;
    let native = canonical.as_os_str().encode_wide().collect::<Vec<_>>();
    if native.starts_with(VERBATIM_UNC_PREFIX) {
        let mut ordinary = vec![b'\\' as u16, b'\\' as u16];
        ordinary.extend_from_slice(&native[VERBATIM_UNC_PREFIX.len()..]);
        return Ok(PathBuf::from(OsString::from_wide(&ordinary)));
    }
    if native.starts_with(VERBATIM_PREFIX) {
        return Ok(PathBuf::from(OsString::from_wide(
            &native[VERBATIM_PREFIX.len()..],
        )));
    }
    Ok(canonical)
}

#[cfg(windows)]
fn lower_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(any(windows, test))]
fn validate_sha256(
    value: &str,
    kind: QualifiedFfmpegToolKind,
    field: &'static str,
) -> Result<(), QualifiedFfmpegToolchainError> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(QualifiedFfmpegToolchainError::InvalidSha256 { kind, field });
    }
    Ok(())
}

/// Exact FFmpeg toolchain preparation or process-installation failure.
#[derive(Debug, Error)]
pub enum QualifiedFfmpegToolchainError {
    /// Descriptor/object-based execution has not yet been qualified here.
    #[error("exact FFmpeg executable ownership is not qualified on this platform")]
    UnsupportedPlatform,
    /// A declared digest was not lowercase SHA-256.
    #[error("invalid {kind:?} {field} digest")]
    InvalidSha256 {
        /// Tool whose declaration failed.
        kind: QualifiedFfmpegToolKind,
        /// Invalid declaration field.
        field: &'static str,
    },
    /// Runtime list was empty, unordered, duplicated, or outside the common
    /// executable directory.
    #[error("invalid packaged FFmpeg runtime-DLL closure")]
    InvalidRuntimeClosure,
    /// Filesystem runtime DLLs did not equal the complete declaration.
    #[error(
        "packaged FFmpeg runtime-DLL closure mismatch: declared {declared}, discovered {discovered}"
    )]
    RuntimeClosureMismatch { declared: usize, discovered: usize },
    /// Packaged runtime directory could not be enumerated.
    #[error("could not inspect packaged FFmpeg runtime directory {path}: {source}")]
    RuntimeDirectoryRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// One runtime path was not already canonical.
    #[error("FFmpeg runtime path is not canonical: {path}")]
    RuntimePathNotCanonical { path: PathBuf },
    /// One runtime source could not be retained.
    #[error("could not open FFmpeg runtime file {path}: {source}")]
    RuntimeSourceOpen {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// One runtime source exceeded its fixed bound.
    #[error("FFmpeg runtime file {path} has unsupported size {actual}")]
    InvalidRuntimeFileSize { path: PathBuf, actual: u64 },
    /// Runtime closure exceeded the fixed aggregate byte bound.
    #[error("FFmpeg runtime-DLL closure exceeds its fixed aggregate byte bound")]
    RuntimeClosureTooLarge,
    /// One runtime DLL could not be published to the private capsule.
    #[error("could not publish FFmpeg runtime snapshot {path}: {source}")]
    RuntimeSnapshotWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// One runtime DLL snapshot could not be retained.
    #[error("could not retain FFmpeg runtime snapshot {path}: {source}")]
    RuntimeSnapshotOpen {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A runtime source or snapshot digest differed from the declaration.
    #[error("FFmpeg runtime file {path} SHA-256 mismatch: expected {expected}, observed {actual}")]
    RuntimeHashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// The loaded in-process libraries did not resolve to the exact retained
    /// packaged runtime closure.
    #[error("linked FFmpeg runtime identity mismatch: {0}")]
    LinkedRuntime(String),
    /// The private executable capsule no longer has its admitted exact entry set.
    #[error("private FFmpeg capsule namespace changed after admission")]
    CapsuleNamespaceChanged,
    /// Exact runtime passed byte identity but not Mondrian's production codec,
    /// filter, muxer, decoder, redistribution, or encoder-option baseline.
    #[error("qualified FFmpeg runtime lacks required semantics: {0}")]
    CommandSemantic(String),
    /// The supplied source path was not already canonical.
    #[error("{kind:?} executable path is not canonical")]
    SourcePathNotCanonical { kind: QualifiedFfmpegToolKind },
    /// The source executable could not be opened as a direct retained object.
    #[error("could not open exact {kind:?} source executable: {source}")]
    SourceOpen {
        kind: QualifiedFfmpegToolKind,
        #[source]
        source: std::io::Error,
    },
    /// Source size was empty or beyond the executable policy.
    #[error("{kind:?} executable size {actual} is outside the supported bound")]
    InvalidExecutableSize {
        kind: QualifiedFfmpegToolKind,
        actual: u64,
    },
    /// The private snapshot directory could not be created.
    #[error("could not create private FFmpeg snapshot directory: {0}")]
    SnapshotDirectory(std::io::Error),
    /// Snapshot publication failed.
    #[error("could not publish exact {kind:?} executable snapshot: {source}")]
    SnapshotWrite {
        kind: QualifiedFfmpegToolKind,
        #[source]
        source: std::io::Error,
    },
    /// Snapshot could not be reopened and retained.
    #[error("could not retain exact {kind:?} executable snapshot: {source}")]
    SnapshotOpen {
        kind: QualifiedFfmpegToolKind,
        #[source]
        source: std::io::Error,
    },
    /// Source executable bytes did not match the machine plan.
    #[error("{kind:?} source executable SHA-256 mismatch: expected {expected}, observed {actual}")]
    ExecutableHashMismatch {
        kind: QualifiedFfmpegToolKind,
        expected: String,
        actual: String,
    },
    /// Private snapshot bytes did not match the retained source object.
    #[error("{kind:?} snapshot SHA-256 mismatch: expected {expected}, observed {actual}")]
    SnapshotHashMismatch {
        kind: QualifiedFfmpegToolKind,
        expected: String,
        actual: String,
    },
    /// A monotonic probe deadline could not be represented.
    #[error("{kind:?} probe deadline overflowed")]
    ProbeDeadlineOverflow { kind: QualifiedFfmpegToolKind },
    /// A fixed probe could not complete under bounded supervision.
    #[cfg(windows)]
    #[error("{kind:?} fixed probe process failed: {source}")]
    ProbeProcess {
        kind: QualifiedFfmpegToolKind,
        #[source]
        source: SupervisedProcessError,
    },
    /// A fixed probe returned a non-success terminal status.
    #[error("{kind:?} probe '{arguments}' failed with {status}: {stderr}")]
    ProbeFailed {
        kind: QualifiedFfmpegToolKind,
        arguments: String,
        status: String,
        stderr: String,
    },
    /// Version stdout did not match the approved identity.
    #[error("{kind:?} version-output SHA-256 mismatch: expected {expected}, observed {actual}")]
    VersionOutputMismatch {
        kind: QualifiedFfmpegToolKind,
        expected: String,
        actual: String,
    },
    /// Fixed framed capability output exceeded its bound.
    #[error("{kind:?} capability report exceeded its fixed memory bound")]
    CapabilityReportTooLarge { kind: QualifiedFfmpegToolKind },
    /// Capability report did not match the approved identity.
    #[error("{kind:?} capability-report SHA-256 mismatch: expected {expected}, observed {actual}")]
    CapabilityReportMismatch {
        kind: QualifiedFfmpegToolKind,
        expected: String,
        actual: String,
    },
    /// Another exact identity was already installed in this process.
    #[error("a different exact FFmpeg toolchain is already installed in this process")]
    ProcessIdentityConflict,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_declarations_are_strict_lowercase_hex() {
        assert!(validate_sha256(&"a".repeat(64), QualifiedFfmpegToolKind::Ffmpeg, "test").is_ok());
        assert!(validate_sha256(&"A".repeat(64), QualifiedFfmpegToolKind::Ffmpeg, "test").is_err());
        assert!(validate_sha256(&"a".repeat(63), QualifiedFfmpegToolKind::Ffmpeg, "test").is_err());
    }

    #[test]
    fn capability_argument_order_is_fixed_and_role_specific() {
        let ffmpeg = QualifiedFfmpegToolKind::Ffmpeg.capability_arguments();
        let ffprobe = QualifiedFfmpegToolKind::Ffprobe.capability_arguments();
        assert!(ffmpeg.contains(&&["-hide_banner", "-encoders"][..]));
        assert!(ffmpeg.contains(&&["-hide_banner", "-hwaccels"][..]));
        assert!(!ffprobe.contains(&&["-hide_banner", "-encoders"][..]));
        assert_eq!(ffprobe.first(), Some(&&["-hide_banner", "-buildconf"][..]));
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_copy_hashes_the_same_open_source_object() {
        let root = tempfile::tempdir().expect("temporary tool root");
        let source_path = root.path().join("source.exe");
        fs::write(&source_path, b"exact executable bytes").expect("write source");
        let source_length = fs::metadata(&source_path).expect("source metadata").len();
        let mut source = open_direct_read_file(&source_path).expect("open source");
        let destination_path = root.path().join("snapshot.exe");
        let mut destination =
            create_direct_exclusive_file(&destination_path).expect("create snapshot");
        let digest =
            copy_and_hash(&mut source, &mut destination, source_length).expect("copy exact bytes");
        destination.sync_all().expect("sync snapshot");
        drop(destination);
        let mut snapshot = open_direct_read_file(&destination_path).expect("open snapshot");
        assert_eq!(digest, lower_sha256(b"exact executable bytes"));
        assert_eq!(
            hash_bounded_file(&mut snapshot, source_length).expect("hash snapshot"),
            digest
        );
    }

    #[cfg(windows)]
    #[test]
    fn runtime_snapshot_requires_the_complete_directory_dll_closure() {
        let source_root = tempfile::tempdir().expect("temporary runtime root");
        let snapshot_root = tempfile::tempdir().expect("temporary snapshot root");
        let ffmpeg = source_root.path().join("ffmpeg.exe");
        let ffprobe = source_root.path().join("ffprobe.exe");
        fs::write(&ffmpeg, b"ffmpeg").expect("write ffmpeg");
        fs::write(&ffprobe, b"ffprobe").expect("write ffprobe");
        let first = source_root.path().join("avcodec-61.dll");
        let second = source_root.path().join("avutil-59.dll");
        fs::write(&first, b"codec").expect("write first DLL");
        fs::write(&second, b"util").expect("write second DLL");
        let ffmpeg = ordinary_canonical_path(&ffmpeg).expect("canonical ffmpeg");
        let ffprobe = ordinary_canonical_path(&ffprobe).expect("canonical ffprobe");
        let first = ordinary_canonical_path(&first).expect("canonical first DLL");
        let second = ordinary_canonical_path(&second).expect("canonical second DLL");
        let only_first = [QualifiedFfmpegRuntimeFileExpectation {
            path: &first,
            sha256: &lower_sha256(b"codec"),
        }];
        assert!(matches!(
            prepare_runtime_files(&ffmpeg, &ffprobe, &only_first, snapshot_root.path()),
            Err(QualifiedFfmpegToolchainError::RuntimeClosureMismatch { .. })
        ));

        let expectations = [
            QualifiedFfmpegRuntimeFileExpectation { path: &first, sha256: &lower_sha256(b"codec") },
            QualifiedFfmpegRuntimeFileExpectation { path: &second, sha256: &lower_sha256(b"util") },
        ];
        let prepared =
            prepare_runtime_files(&ffmpeg, &ffprobe, &expectations, snapshot_root.path())
                .expect("prepare exact runtime closure");
        assert_eq!(prepared.len(), 2);
        assert!(OpenOptions::new().write(true).open(&first).is_err());
        assert!(OpenOptions::new()
            .write(true)
            .open(snapshot_root.path().join("avcodec-61.dll"))
            .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn retained_capsule_directory_cannot_be_renamed() {
        let owner = tempfile::tempdir().expect("temporary owner root");
        let capsule = owner.path().join("capsule");
        fs::create_dir(&capsule).expect("create capsule");
        let lease = open_direct_read_directory(&capsule).expect("retain capsule");
        fs::write(capsule.join("child.bin"), b"child")
            .expect("create child under retained capsule");
        assert!(fs::rename(&capsule, owner.path().join("moved")).is_err());
        drop(lease);
        fs::rename(&capsule, owner.path().join("moved")).expect("rename after release");
    }

    #[cfg(windows)]
    #[test]
    fn capsule_namespace_revalidation_rejects_inserted_entries() {
        let root = tempfile::tempdir().expect("temporary capsule root");
        fs::write(root.path().join("ffmpeg.exe"), b"ffmpeg").expect("write expected entry");
        let expected =
            [std::ffi::OsString::from("ffmpeg.exe")].into_iter().collect::<BTreeSet<_>>();
        assert!(capsule_directory_matches(root.path(), &expected));
        fs::write(root.path().join("poison.dll"), b"poison").expect("insert capsule entry");
        assert!(!capsule_directory_matches(root.path(), &expected));
    }

    #[cfg(windows)]
    #[test]
    fn direct_open_rejects_file_and_directory_reparse_points() {
        use std::os::windows::fs::{symlink_dir, symlink_file};

        let root = tempfile::tempdir().expect("temporary reparse root");
        let target_file = root.path().join("target.bin");
        let linked_file = root.path().join("linked.bin");
        fs::write(&target_file, b"target").expect("write target file");
        if symlink_file(&target_file, &linked_file).is_ok() {
            assert!(open_direct_read_file(&linked_file).is_err());
        }

        let target_directory = root.path().join("target-directory");
        let linked_directory = root.path().join("linked-directory");
        fs::create_dir(&target_directory).expect("create target directory");
        if symlink_dir(&target_directory, &linked_directory).is_ok() {
            assert!(open_direct_read_directory(&linked_directory).is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn loaded_module_admission_requires_an_exact_declared_or_system_path() {
        let approved_directory = PathBuf::from(r"C:\approved");
        let declared = approved_directory.join("avcodec-61.dll");
        let undeclared = approved_directory.join("poison.dll");
        let system_directory = PathBuf::from(r"C:\Windows\System32");
        let approved = [declared.clone()].into_iter().collect::<BTreeSet<_>>();
        assert!(module_path_is_admitted(
            &declared,
            &approved,
            std::slice::from_ref(&system_directory)
        ));
        assert!(!module_path_is_admitted(
            &undeclared,
            &approved,
            std::slice::from_ref(&system_directory)
        ));
        assert!(module_path_is_admitted(
            &system_directory.join("kernel32.dll"),
            &approved,
            std::slice::from_ref(&system_directory)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn process_install_rejects_a_previously_poisoned_capsule() {
        let capsule = tempfile::tempdir().expect("temporary capsule");
        let ffmpeg_path = capsule.path().join("ffmpeg.exe");
        let ffprobe_path = capsule.path().join("ffprobe.exe");
        fs::write(&ffmpeg_path, b"ffmpeg").expect("write ffmpeg snapshot");
        fs::write(&ffprobe_path, b"ffprobe").expect("write ffprobe snapshot");
        let make_tool = |kind, path: &Path| PreparedFfmpegTool {
            receipt: QualifiedFfmpegToolReceipt {
                kind,
                source_path: path.to_path_buf(),
                executable_sha256: "a".repeat(64),
                snapshot_sha256: "a".repeat(64),
                version_output_sha256: "b".repeat(64),
                capability_report_sha256: "c".repeat(64),
            },
            snapshot_path: path.to_path_buf(),
            snapshot_directory: capsule.path().to_path_buf(),
            _source_lease: File::open(path).expect("open source lease"),
            _snapshot_lease: File::open(path).expect("open snapshot lease"),
        };
        let directory_lease =
            open_direct_read_directory(capsule.path()).expect("retain capsule directory");
        fs::write(capsule.path().join("poison.dll"), b"poison").expect("poison capsule");
        let toolchain = Arc::new(PreparedFfmpegToolchain {
            ffmpeg: make_tool(QualifiedFfmpegToolKind::Ffmpeg, &ffmpeg_path),
            ffprobe: make_tool(QualifiedFfmpegToolKind::Ffprobe, &ffprobe_path),
            runtime_files: Vec::new(),
            _snapshot_directory_lease: directory_lease,
            _snapshot_directory: capsule,
            namespace_poisoned: AtomicBool::new(false),
        });
        assert!(matches!(
            install_process_ffmpeg_toolchain(toolchain),
            Err(QualifiedFfmpegToolchainError::CapsuleNamespaceChanged)
        ));
        assert!(PROCESS_TOOLCHAIN.get().is_none());
    }
}
