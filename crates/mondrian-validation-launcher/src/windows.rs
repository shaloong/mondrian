//! Windows-only filesystem-race-resistant launcher and authenticated bootstrap.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    SetNamedPipeHandleState,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};

const PIPE_ENV: &str = "MONDRIAN_NATIVE_PRELOADER_PIPE_V1";
const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_MESSAGE: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    attestation: PreloaderAttestation,
    capsule: PathBuf,
    namespace_sddl: String,
}

/// Retained post-handshake object authority. Construction requires a native peer.
#[derive(Debug)]
pub struct PreparedAuthority {
    receipt: PreloaderAttestation,
    capsule: PathBuf,
    namespace_sddl: String,
    _objects: Vec<File>,
    _peer: File,
}
impl PreparedAuthority {
    /// Raw native handshake evidence; caller-supplied values alone cannot construct an owner.
    pub fn receipt(&self) -> &PreloaderAttestation {
        &self.receipt
    }
    /// Exact staged application image, approved before loading any child DLL.
    pub fn application_path(&self) -> &Path {
        &self.receipt.owned_images[0].staged_path
    }
    /// Revalidate the namespace policy; retained object handles deny write/delete races.
    pub fn validate(&self) -> Result<(), LaunchError> {
        crate::namespace::verify_sealed_namespace(&self.capsule, &self.namespace_sddl)?;
        Ok(())
    }
    /// Translate an approved source to its retained pre-loader mapped object.
    pub fn mapped_path(&self, path: &Path, sha256: &str) -> Option<&Path> {
        self.receipt
            .owned_images
            .iter()
            .find(|item| item.source.path == path && item.source.sha256 == sha256)
            .map(|item| item.staged_path.as_path())
    }
}

fn invalid(message: impl Into<String>) -> LaunchError {
    LaunchError(message.into())
}
fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn canonical(path: &Path) -> Result<PathBuf, LaunchError> {
    if !path.is_absolute() {
        return Err(invalid("expected absolute direct path"));
    }
    Ok(path.canonicalize()?)
}
fn direct(path: &Path) -> Result<File, LaunchError> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .custom_flags(0x0020_0000)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.file_attributes() & 0x400 != 0
        || metadata.len() == 0
        || metadata.len() > MAX_FILE
    {
        return Err(invalid("file is not a bounded non-reparse object"));
    }
    Ok(file)
}
fn object(file: &File) -> Result<FileIdentity, LaunchError> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(FileIdentity {
        volume_serial: info.dwVolumeSerialNumber,
        file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        length: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
    })
}
fn hash(file: &mut File) -> Result<String, LaunchError> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_FILE {
            return Err(invalid("hash size exceeded"));
        }
        digest.update(&bytes[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn verify_binding(binding: &FileBinding) -> Result<File, LaunchError> {
    if binding.sha256.len() != 64
        || !binding.sha256.bytes().all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(invalid("invalid lowercase SHA-256"));
    }
    let mut file = direct(&binding.path)?;
    if hash(&mut file)? != binding.sha256 {
        return Err(invalid(format!(
            "full-file identity mismatch: {}",
            binding.path.display()
        )));
    }
    Ok(file)
}
fn system_directory() -> Result<PathBuf, LaunchError> {
    let mut buffer = vec![0_u16; 32768];
    let count = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } as usize;
    if count == 0 || count >= buffer.len() {
        return Err(invalid("cannot obtain native system directory"));
    }
    canonical(Path::new(&std::ffi::OsString::from_wide(&buffer[..count])))
}
fn process_image(pid: u32) -> Result<(PathBuf, File), LaunchError> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let process = unsafe { File::from_raw_handle(handle) };
    let mut buffer = vec![0_u16; 32768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((
        canonical(Path::new(&std::ffi::OsString::from_wide(
            &buffer[..length as usize],
        )))?,
        process,
    ))
}
fn mapped_paths() -> Result<Vec<PathBuf>, LaunchError> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32EnumProcessModules, K32GetModuleFileNameExW,
    };
    let mut modules = vec![std::ptr::null_mut(); 4096];
    let mut needed = 0;
    let process = unsafe { GetCurrentProcess() };
    let bytes = (modules.len() * std::mem::size_of::<*mut std::ffi::c_void>()) as u32;
    if unsafe { K32EnumProcessModules(process, modules.as_mut_ptr(), bytes, &mut needed) } == 0
        || needed > bytes
    {
        return Err(invalid(
            "native module enumeration failed or exceeded bound",
        ));
    }
    let mut paths = Vec::new();
    for module in modules
        .into_iter()
        .take(needed as usize / std::mem::size_of::<*mut std::ffi::c_void>())
    {
        let mut buffer = vec![0_u16; 32768];
        let count = unsafe {
            K32GetModuleFileNameExW(process, module, buffer.as_mut_ptr(), buffer.len() as u32)
        } as usize;
        if count == 0 || count >= buffer.len() {
            return Err(invalid("native mapped path readback failed"));
        }
        paths.push(canonical(Path::new(&std::ffi::OsString::from_wide(
            &buffer[..count],
        )))?);
    }
    Ok(paths)
}
fn transfer(
    file: &mut File,
    bytes: &mut [u8],
    writing: bool,
    deadline: Instant,
) -> Result<(), LaunchError> {
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= deadline {
            return Err(invalid("original bootstrap deadline exceeded"));
        }
        // std::fs::File::read translates ERROR_NO_DATA to Ok(0) on Windows.
        // For PIPE_NOWAIT this is an empty live pipe, not EOF. Preserve the
        // native status so a delayed peer cannot be mistaken for a closed peer.
        let mut count = 0_u32;
        let length = u32::try_from(bytes.len() - offset)
            .map_err(|_| invalid("bootstrap transfer length exceeded"))?;
        let succeeded = unsafe {
            if writing {
                windows_sys::Win32::Storage::FileSystem::WriteFile(
                    file.as_raw_handle(),
                    bytes[offset..].as_ptr(),
                    length,
                    &mut count,
                    std::ptr::null_mut(),
                )
            } else {
                windows_sys::Win32::Storage::FileSystem::ReadFile(
                    file.as_raw_handle(),
                    bytes[offset..].as_mut_ptr(),
                    length,
                    &mut count,
                    std::ptr::null_mut(),
                )
            }
        };
        if succeeded == 0 {
            let error = std::io::Error::last_os_error();
            if !writing && error.raw_os_error() == Some(232) {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            return Err(invalid(format!(
                "bootstrap native {} at {offset}: {error}",
                if writing { "write" } else { "read" }
            )));
        }
        if count == 0 {
            if writing {
                // A byte-mode nonblocking write may exhaust the pipe quota.
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            return Err(invalid("bootstrap pipe closed before complete frame"));
        }
        offset += count as usize;
    }
    Ok(())
}
fn write_frame<T: Serialize>(
    file: &mut File,
    value: &T,
    deadline: Instant,
) -> Result<(), LaunchError> {
    let mut body = serde_json::to_vec(value)?;
    if body.len() > MAX_MESSAGE {
        return Err(invalid("bootstrap frame exceeds bound"));
    }
    transfer(file, &mut (body.len() as u32).to_le_bytes(), true, deadline)?;
    transfer(file, &mut body, true, deadline)
}
fn read_frame<T: serde::de::DeserializeOwned>(
    file: &mut File,
    deadline: Instant,
) -> Result<T, LaunchError> {
    let mut prefix = [0_u8; 4];
    transfer(file, &mut prefix, false, deadline)?;
    let size = u32::from_le_bytes(prefix) as usize;
    if size == 0 || size > MAX_MESSAGE {
        return Err(invalid("bootstrap frame length rejected"));
    }
    let mut body = vec![0; size];
    transfer(file, &mut body, false, deadline)?;
    Ok(serde_json::from_slice(&body)?)
}

/// Authenticate a real native pipe peer, then retain every approved mapped object.
/// An absent bootstrap is explicitly unavailable and never yields a fabricated owner.
pub fn attest(
    expected: AttestationExpectation<'_>,
) -> Result<Option<PreparedAuthority>, LaunchError> {
    let Some(pipe_name) = std::env::var_os(PIPE_ENV) else {
        return Ok(None);
    };
    let pipe_text = pipe_name.to_str().ok_or_else(|| invalid("bootstrap name is not UTF-8"))?;
    if !pipe_text.starts_with(r"\\.\pipe\mondrian-preloader-") || pipe_text.len() > 150 {
        return Err(invalid("invalid local bootstrap pipe name"));
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut pipe = OpenOptions::new().read(true).write(true).open(&pipe_name)?;
    let mode = 1_u32; // PIPE_NOWAIT; no worker can hide a blocking handshake.
    if unsafe {
        SetNamedPipeHandleState(
            pipe.as_raw_handle(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut server_pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut server_pid) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let (server_path, server_process) = process_image(server_pid)?;
    if server_path != canonical(&expected.launcher.path)? {
        return Err(invalid(
            "native pipe server image path differs from external approval",
        ));
    }
    let server_image = verify_binding(expected.launcher)?;
    let mut offer: Offer = read_frame(&mut pipe, deadline)?;
    if offer.attestation.schema_version != 1
        || offer.attestation.launcher_pid != server_pid
        || offer.attestation.child_pid != std::process::id()
        || offer.attestation.launcher_sha256 != expected.launcher.sha256
        || offer.attestation.request_sha256 != expected.request_sha256
        || offer.attestation.machine_plan_sha256 != expected.machine_plan_sha256
        || uuid::Uuid::parse_str(&offer.attestation.challenge).is_err()
    {
        return Err(invalid(
            "bootstrap peer, challenge, or approved request identity mismatch",
        ));
    }
    crate::namespace::verify_sealed_namespace(&offer.capsule, &offer.namespace_sddl)?;
    let mut expected_files: BTreeMap<PathBuf, String> = expected
        .runtime_files
        .iter()
        .map(|file| (file.path.clone(), file.sha256.clone()))
        .collect();
    if expected_files.len() != expected.runtime_files.len() {
        return Err(invalid("duplicate expected runtime source"));
    }
    let current = canonical(&std::env::current_exe()?)?;
    let mut objects = vec![server_image, server_process];
    let mut approved_paths = BTreeSet::new();
    if offer
        .attestation
        .owned_images
        .first()
        .is_none_or(|image| image.staged_path != current)
    {
        return Err(invalid("first native image is not the application"));
    }
    let mut application_seen = false;
    for image in &offer.attestation.owned_images {
        if image.staged_path.parent() != Some(offer.capsule.as_path())
            || canonical(&image.staged_path)? != image.staged_path
            || !approved_paths.insert(image.staged_path.clone())
        {
            return Err(invalid(
                "staged image escaped sealed namespace or was duplicated",
            ));
        }
        if image.staged_path == current {
            if application_seen || image.source.sha256 != expected.application_sha256 {
                return Err(invalid(
                    "application image differs from approved runtime identity",
                ));
            }
            application_seen = true;
        } else if expected_files.remove(&image.source.path).as_deref() != Some(&image.source.sha256)
        {
            return Err(invalid(
                "staged runtime source differs from approved machine closure",
            ));
        }
        let file = verify_binding(&FileBinding {
            path: image.staged_path.clone(),
            sha256: image.source.sha256.clone(),
        })?;
        if object(&file)? != image.object {
            return Err(invalid("staged native file object changed"));
        }
        objects.push(file);
    }
    if !application_seen || !expected_files.is_empty() {
        return Err(invalid(
            "bootstrap omitted an approved application/runtime object",
        ));
    }
    let system = system_directory()?;
    let mapped = mapped_paths()?;
    for path in &mapped {
        if !approved_paths.contains(path) && !path.starts_with(&system) {
            return Err(invalid(format!(
                "non-system mapped module not owned before loader: {}",
                path.display()
            )));
        }
    }
    if !mapped.contains(&current) {
        return Err(invalid("native application mapping not observed"));
    }
    offer.attestation.mapped_image_paths =
        mapped.into_iter().filter(|path| approved_paths.contains(path)).collect();
    write_frame(&mut pipe, &offer.attestation, deadline)?;
    let accepted: String = read_frame(&mut pipe, deadline)?;
    if accepted != offer.attestation.challenge {
        return Err(invalid("launcher did not acknowledge this native child"));
    }
    Ok(Some(PreparedAuthority {
        capsule: offer.capsule,
        namespace_sddl: offer.namespace_sddl,
        receipt: offer.attestation,
        _objects: objects,
        _peer: pipe,
    }))
}

struct Capsule {
    root: Option<tempfile::TempDir>,
    seal: Option<crate::namespace::CapsuleSeal>,
    objects: Vec<File>,
    ancestors: Vec<File>,
    staged_bytes: u64,
}
impl Capsule {
    fn new() -> Result<Self, LaunchError> {
        let base = canonical(&std::env::temp_dir())?;
        let mut ancestors = Vec::new();
        for path in base.ancestors() {
            ancestors.push(
                OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .custom_flags(0x0220_0000)
                    .open(path)?,
            );
        }
        Ok(Self {
            root: Some(tempfile::Builder::new().prefix("mondrian-preloader-").tempdir_in(base)?),
            seal: None,
            objects: Vec::new(),
            ancestors,
            staged_bytes: 0,
        })
    }
    fn path(&self) -> &Path {
        self.root.as_ref().expect("capsule not consumed").path()
    }
    fn stage(&mut self, source: &FileBinding) -> Result<MappedImageEvidence, LaunchError> {
        let mut input = verify_binding(source)?;
        let size = input.metadata()?.len();
        self.staged_bytes = self
            .staged_bytes
            .checked_add(size)
            .filter(|size| *size <= 8 * 1024 * 1024 * 1024)
            .ok_or_else(|| invalid("aggregate pre-loader capsule exceeds eight GiB"))?;
        let name = source.path.file_name().ok_or_else(|| invalid("missing image leaf"))?;
        let path = self.path().join(name);
        let mut output =
            OpenOptions::new().write(true).create_new(true).share_mode(0).open(&path)?;
        input.seek(SeekFrom::Start(0))?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        let staged =
            verify_binding(&FileBinding { path: path.clone(), sha256: source.sha256.clone() })?;
        let evidence = MappedImageEvidence {
            source: source.clone(),
            staged_path: canonical(&path)?,
            object: object(&staged)?,
        };
        self.objects.extend([input, staged]);
        Ok(evidence)
    }
    fn close(&mut self, errors: &mut Vec<String>) -> bool {
        if let Some(mut seal) = self.seal.take() {
            if let Err(error) = seal.validate() {
                errors.push(format!("namespace readback: {error}"));
            }
            if let Err(error) = seal.restore() {
                errors.push(format!("namespace restoration: {error}"));
            }
            drop(seal);
        }
        self.objects.clear();
        let removed = match self.root.take() {
            Some(root) => match root.close() {
                Ok(()) => true,
                Err(error) => {
                    errors.push(format!("capsule removal: {error}"));
                    false
                }
            },
            None => false,
        };
        self.ancestors.clear();
        removed
    }
}
impl Drop for Capsule {
    fn drop(&mut self) {
        if self.root.is_some() {
            let mut errors = Vec::new();
            self.close(&mut errors);
            for error in errors {
                tracing::error!(%error, "unconsumed launcher capsule cleanup");
            }
        }
    }
}

/// Seal all approved images before process creation and consume the outer owner.
pub fn launch(plan: &LaunchPlan) -> Result<LaunchReport, LaunchError> {
    if plan.schema_version != 1
        || plan.runtime_files.is_empty()
        || plan.runtime_files.len() > 512
        || plan.deadline_ms == 0
        || plan.deadline_ms > 7 * 24 * 60 * 60 * 1000
        || plan.report_path.exists()
        || !plan.report_path.is_absolute()
    {
        return Err(invalid(
            "invalid launch plan version, bounds, or create-only report destination",
        ));
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(plan.deadline_ms))
        .ok_or_else(|| invalid("deadline overflow"))?;
    let launcher = verify_binding(&plan.launcher)?;
    if canonical(&plan.launcher.path)? != canonical(&std::env::current_exe()?)? {
        return Err(invalid("plan launcher is not the executing native image"));
    }
    let mut request = verify_binding(&plan.request)?;
    let request_json = read_json_file(&mut request)?;
    let machine_binding: FileBinding =
        serde_json::from_value(request_json["machine_plan"].clone())?;
    let mut machine = verify_binding(&machine_binding)?;
    let machine_json = read_json_file(&mut machine)?;
    let approved_launcher: FileBinding =
        serde_json::from_value(machine_json["verifier_tools"]["preloader"].clone())?;
    let approved_runtime: Vec<FileBinding> =
        serde_json::from_value(machine_json["verifier_tools"]["runtime_files"].clone())?;
    if approved_launcher != plan.launcher
        || approved_runtime != plan.runtime_files
        || request_json["identity"]["runtime_image_sha256"].as_str()
            != Some(&plan.application.sha256)
    {
        return Err(invalid(
            "launch images differ from the request's external machine plan",
        ));
    }
    let manifest_path = PathBuf::from(
        request_json["output_manifest_path"]
            .as_str()
            .ok_or_else(|| invalid("strict request omitted output manifest"))?,
    );
    if !manifest_path.is_absolute() || manifest_path.exists() {
        return Err(invalid(
            "child manifest destination is not absolute/create-only",
        ));
    }
    let system = system_directory()?;
    for image in mapped_paths()? {
        if image != canonical(&plan.launcher.path)? && !image.starts_with(&system) {
            return Err(invalid(
                "FFmpeg-free launcher has an unapproved non-system import",
            ));
        }
    }
    let mut capsule = Capsule::new()?;
    capsule.objects.extend([launcher, request, machine]);
    let mut names = BTreeSet::new();
    for binding in std::iter::once(&plan.application).chain(&plan.runtime_files) {
        let name = binding
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("invalid native image leaf"))?
            .to_ascii_lowercase();
        if !names.insert(name) {
            return Err(invalid("case-insensitive image leaf collision"));
        }
    }
    let mut images = vec![capsule.stage(&plan.application)?];
    for runtime in &plan.runtime_files {
        if Instant::now() >= deadline {
            return Err(invalid("original launch deadline expired during staging"));
        }
        images.push(capsule.stage(runtime)?);
    }
    let root = canonical(capsule.path())?;
    capsule.seal = Some(crate::namespace::CapsuleSeal::seal(&root)?);
    validate_namespace_contents(&root, &images)?;
    let challenge = uuid::Uuid::new_v4().to_string();
    let pipe_name = format!(r"\\.\pipe\mondrian-preloader-{challenge}");
    let name = wide(std::ffi::OsStr::new(&pipe_name));
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            3 | 0x0008_0000,
            1 | 8,
            1,
            MAX_MESSAGE as u32,
            MAX_MESSAGE as u32,
            0,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut pipe = unsafe { File::from_raw_handle(handle) };
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(&images[0].staged_path);
    command
        .arg(&plan.request.path)
        .env_clear()
        .current_dir(&root)
        .env(
            "SystemRoot",
            system.parent().ok_or_else(|| invalid("system root unavailable"))?,
        )
        .env("PATH", &root)
        .env(PIPE_ENV, pipe_name)
        .creation_flags(0x0800_0000 | 4);
    command
        .env("TEMP", canonical(&std::env::temp_dir())?)
        .env("TMP", canonical(&std::env::temp_dir())?);
    let job = Job::new()?;
    if Instant::now() >= deadline {
        return Err(invalid(
            "original launch deadline expired before native spawn",
        ));
    }
    let mut child = command.spawn()?;
    let mut report = LaunchReport {
        schema_version: 1,
        attestation: None,
        child_manifest: None,
        exit_code: None,
        deadline_exceeded: false,
        capsule_removed: false,
        descendants_reaped: false,
        errors: Vec::new(),
    };
    let bootstrap_deadline = deadline.min(Instant::now() + Duration::from_secs(30));
    let handshake = (|| -> Result<(), LaunchError> {
        job.assign_and_resume(&child)?;
        loop {
            if unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0 {
                break;
            }
            let error = unsafe { GetLastError() };
            if error == 535 {
                break;
            }
            if error != 536 && error != 232 {
                return Err(std::io::Error::from_raw_os_error(error as i32).into());
            }
            if Instant::now() >= bootstrap_deadline || child.try_wait()?.is_some() {
                return Err(invalid(
                    "child exited or bootstrap deadline expired before authenticated connection",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut peer_pid = 0;
        if unsafe { GetNamedPipeClientProcessId(handle, &mut peer_pid) } == 0
            || peer_pid != child.id()
        {
            return Err(invalid(
                "bootstrap pipe client is not the retained native child",
            ));
        }
        let attestation = PreloaderAttestation {
            schema_version: 1,
            launcher_pid: std::process::id(),
            child_pid: child.id(),
            launcher_sha256: plan.launcher.sha256.clone(),
            request_sha256: plan.request.sha256.clone(),
            machine_plan_sha256: machine_binding.sha256.clone(),
            challenge,
            owned_images: images,
            mapped_image_paths: Vec::new(),
        };
        let seal = capsule.seal.as_ref().ok_or_else(|| invalid("namespace authority missing"))?;
        let offer = Offer {
            attestation,
            capsule: root,
            namespace_sddl: seal.descriptor().to_owned(),
        };
        write_frame(&mut pipe, &offer, bootstrap_deadline)?;
        let response: PreloaderAttestation = read_frame(&mut pipe, bootstrap_deadline)?;
        let mut expected_response = offer.attestation.clone();
        expected_response.mapped_image_paths = response.mapped_image_paths.clone();
        let admitted = expected_response
            .owned_images
            .iter()
            .map(|image| &image.staged_path)
            .collect::<BTreeSet<_>>();
        let mapped = response.mapped_image_paths.iter().collect::<BTreeSet<_>>();
        if response != expected_response
            || mapped.len() != response.mapped_image_paths.len()
            || !mapped.contains(&expected_response.owned_images[0].staged_path)
            || !mapped.is_subset(&admitted)
        {
            return Err(invalid("child native module acknowledgement mismatch"));
        }
        seal.validate()?;
        write_frame(&mut pipe, &response.challenge, bootstrap_deadline)?;
        report.attestation = Some(response);
        Ok(())
    })();
    if let Err(error) = handshake {
        report.errors.push(error.to_string());
    }
    let mut kill_requested = false;
    let mut job_settled = false;
    let mut wait_failed = false;
    let mut accounting_failed = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                report.exit_code = status.code();
            }
            Err(error) => {
                if !wait_failed {
                    report.errors.push(format!("native exit observation: {error}"));
                    wait_failed = true;
                }
            }
            Ok(None) => {}
        }
        match job.active() {
            Ok(0) => {
                job_settled = true;
            }
            Ok(_) => {
                job_settled = false;
            }
            Err(error) => {
                if !accounting_failed {
                    report.errors.push(format!("native descendant accounting: {error}"));
                    accounting_failed = true;
                }
            }
        }
        if report.exit_code.is_some() && job_settled {
            break;
        }
        // A root exit with live descendants is a failed owner closure, not success.
        if report.exit_code.is_some() && !job_settled && report.errors.is_empty() {
            report.errors.push("root exited with live native descendants".to_owned());
        }
        if !report.errors.is_empty() || Instant::now() >= deadline {
            report.deadline_exceeded = Instant::now() >= deadline;
            if !kill_requested {
                if let Err(error) = job.terminate() {
                    report.errors.push(format!("native descendant termination: {error}"));
                }
                if report.exit_code.is_none()
                    && let Err(error) = child.kill()
                {
                    report.errors.push(format!("native termination: {error}"));
                }
                kill_requested = true;
            }
            if Instant::now() >= deadline {
                report.errors.push("original deadline expired before native reap".to_owned());
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    report.descendants_reaped = job_settled;
    if report.exit_code.is_some() && job_settled {
        drop(child);
        drop(pipe);
        drop(job);
        report.capsule_removed = capsule.close(&mut report.errors);
        match direct(&manifest_path).and_then(|mut file| hash(&mut file)) {
            Ok(sha256) => {
                report.child_manifest = Some(FileBinding { path: manifest_path, sha256 });
            }
            Err(error) => {
                report.errors.push(format!("completed child manifest: {error}"));
            }
        }
    } else {
        // No completed receipt can release mapped objects while a native owner survives.
        std::mem::forget((capsule, child, pipe, job));
    }
    Ok(report)
}

static PROCESS_AUTHORITY: std::sync::OnceLock<PreparedAuthority> = std::sync::OnceLock::new();

/// Install a native-attested immutable process authority once. Absent transport
/// remains unavailable; a later caller cannot install a different run identity.
pub fn prepare_process_authority(
    expected: AttestationExpectation<'_>,
) -> Result<bool, LaunchError> {
    if let Some(authority) = PROCESS_AUTHORITY.get() {
        if authority.receipt.launcher_sha256 != expected.launcher.sha256
            || authority.receipt.request_sha256 != expected.request_sha256
        {
            return Err(invalid(
                "a different pre-loader authority is already installed",
            ));
        }
        return Ok(true);
    }
    let Some(authority) = attest(expected)? else {
        return Ok(false);
    };
    PROCESS_AUTHORITY
        .set(authority)
        .map_err(|_| invalid("concurrent pre-loader installation"))?;
    Ok(true)
}

/// Borrow native authority only after its authenticated handshake completed.
pub fn process_authority() -> Option<&'static PreparedAuthority> {
    PROCESS_AUTHORITY.get()
}

struct Job(File);
impl Job {
    fn new() -> Result<Self, LaunchError> {
        use windows_sys::Win32::System::JobObjects::*;
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let job = Self(unsafe { File::from_raw_handle(handle) });
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(job)
    }
    fn assign_and_resume(&self, child: &std::process::Child) -> Result<(), LaunchError> {
        use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows_sys::Win32::System::Threading::{
            OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
        };
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.as_raw_handle()) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let _snapshot = unsafe { File::from_raw_handle(snapshot) };
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut present = unsafe { Thread32First(snapshot, &mut entry) };
        let mut thread_id = None;
        while present != 0 {
            if entry.th32OwnerProcessID == child.id()
                && thread_id.replace(entry.th32ThreadID).is_some()
            {
                return Err(invalid("suspended child unexpectedly has multiple threads"));
            }
            present = unsafe { Thread32Next(snapshot, &mut entry) };
        }
        let id =
            thread_id.ok_or_else(|| invalid("suspended native primary thread was not observed"))?;
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, id) };
        if thread.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let _thread = unsafe { File::from_raw_handle(thread) };
        if unsafe { ResumeThread(thread) } != 1 {
            return Err(invalid(
                "native primary thread suspend count was not exactly one",
            ));
        }
        Ok(())
    }
    fn active(&self) -> Result<u32, LaunchError> {
        use windows_sys::Win32::System::JobObjects::*;
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(accounting.ActiveProcesses)
    }
    fn terminate(&self) -> Result<(), LaunchError> {
        if unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(
                self.0.as_raw_handle(),
                0xdead,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

fn read_json_file(file: &mut File) -> Result<serde_json::Value, LaunchError> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(MAX_MESSAGE as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(invalid("bound request/machine JSON exceeds one MiB"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn validate_namespace_contents(
    root: &Path,
    images: &[MappedImageEvidence],
) -> Result<(), LaunchError> {
    let mut expected = images
        .iter()
        .map(|image| (image.staged_path.clone(), &image.object))
        .collect::<BTreeMap<_, _>>();
    if expected.len() != images.len() {
        return Err(invalid("duplicate staged namespace binding"));
    }
    for entry in std::fs::read_dir(root)
        .map_err(|error| invalid(format!("sealed namespace enumeration: {error}")))?
    {
        let entry = entry?;
        let path = canonical(&entry.path()).map_err(|error| {
            invalid(format!(
                "sealed entry path {}: {error}",
                entry.path().display()
            ))
        })?;
        let identity = expected
            .remove(&path)
            .ok_or_else(|| invalid("unapproved object entered capsule before namespace seal"))?;
        if object(&direct(&entry.path())?)? != *identity {
            return Err(invalid("sealed namespace object identity changed"));
        }
    }
    if !expected.is_empty() {
        return Err(invalid("sealed namespace omitted approved files"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn nonblocking_empty_pipe_waits_for_data_but_closed_peer_fails() {
        let name = format!(
            r"\\.\pipe\mondrian-bootstrap-transfer-{}",
            uuid::Uuid::new_v4()
        );
        let native_name = wide(std::ffi::OsStr::new(&name));
        let handle = unsafe {
            CreateNamedPipeW(
                native_name.as_ptr(),
                3 | 0x0008_0000,
                1 | 8,
                1,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        let mut server = unsafe { File::from_raw_handle(handle) };
        let mut client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .expect("connect native client");
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
        assert!(connected != 0 || unsafe { GetLastError() } == 535);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            client.write_all(&[42]).expect("delayed native write");
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut bytes = [0_u8; 1];
        transfer(&mut server, &mut bytes, false, deadline).expect("empty live pipe is not EOF");
        assert_eq!(bytes, [42]);
        writer.join().expect("writer exits and closes pipe");
        let started = Instant::now();
        let error = transfer(&mut server, &mut bytes, false, deadline)
            .expect_err("closed peer must fail without renewing deadline");
        assert!(error.0.contains("native read"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn oversized_or_partial_bootstrap_frames_fail_closed() {
        let mut file = tempfile::tempfile().expect("frame file");
        file.write_all(&((MAX_MESSAGE as u32) + 1).to_le_bytes()).expect("frame prefix");
        file.seek(SeekFrom::Start(0)).expect("rewind");
        assert!(
            read_frame::<String>(&mut file, Instant::now() + Duration::from_secs(1))
                .expect_err("oversized frame")
                .0
                .contains("length")
        );
        file.set_len(2).expect("truncate");
        file.seek(SeekFrom::Start(0)).expect("rewind");
        assert!(
            read_frame::<String>(&mut file, Instant::now() + Duration::from_secs(1))
                .expect_err("partial frame")
                .0
                .contains("closed")
        );
    }
    #[test]
    fn expired_bootstrap_deadline_is_never_renewed() {
        let mut file = tempfile::tempfile().expect("frame file");
        let start = Instant::now();
        let error = read_frame::<String>(&mut file, start).expect_err("original deadline");
        assert!(error.0.contains("deadline"));
        assert!(start.elapsed() < Duration::from_millis(100));
    }
    #[test]
    fn injection_before_seal_is_rejected_before_native_spawn() {
        let mut capsule = Capsule::new().expect("capsule");
        let source_root = tempfile::tempdir().expect("source directory");
        let source = source_root.path().join("app.exe");
        std::fs::write(&source, b"approved executable fixture").expect("source");
        let mut file = direct(&source).expect("source lease");
        let image = capsule
            .stage(&FileBinding {
                path: source,
                sha256: hash(&mut file).expect("source digest"),
            })
            .expect("stage approved image");
        std::fs::write(
            capsule.path().join("injected.dll"),
            b"hostile lookup candidate",
        )
        .expect("pre-seal injection");
        let root = canonical(capsule.path()).expect("capsule path");
        capsule.seal = Some(crate::namespace::CapsuleSeal::seal(&root).expect("namespace seal"));
        let error = validate_namespace_contents(&root, &[image])
            .expect_err("injected namespace must reject");
        assert!(
            error.0.contains("unapproved object"),
            "actual pre-spawn rejection: {error}"
        );
        let mut errors = Vec::new();
        assert!(capsule.close(&mut errors));
        assert!(errors.is_empty());
    }
    #[test]
    fn object_hash_rejects_mismatched_full_file_identity() {
        let root = tempfile::tempdir().expect("fixture directory");
        let path = root.path().join("image.dll");
        std::fs::write(&path, b"actual bytes with untrusted overlay").expect("fixture image");
        let binding = FileBinding { path, sha256: "0".repeat(64) };
        assert!(verify_binding(&binding)
            .expect_err("all bytes hashed")
            .0
            .contains("full-file identity"));
    }
}
