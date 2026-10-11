//! Isolated OpenFX binary identity discovery.
//!
//! This is an ABI entry-point inspection boundary, not image-effect admission:
//! discovery calls the optional early `OfxSetHost` with the actual native host,
//! then inspects headers without `OfxActionLoad` or a render action. Native
//! code runs only in the supervised child dispatched by the product executable.

use std::collections::BTreeSet;
use std::ffi::{c_char, c_int, c_uint, c_void, OsStr};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) mod child_supervision;
use child_supervision::{wait_for_child, ChildWaitError};

use libloading::Library;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Hidden product executable mode for native OpenFX binary inspection.
pub const OPENFX_DISCOVERY_WORKER_ARGUMENT: &str = "--internal-openfx-discovery-v1";
const REQUEST_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_DISCOVERY_REQUEST";
const RESPONSE_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_DISCOVERY_RESPONSE";
const DEADLINE: Duration = Duration::from_secs(10);
const MAX_MESSAGE_BYTES: u64 = 256 * 1024;
const MAX_PLUGINS: c_int = 1024;
const MAX_IDENTIFIER_BYTES: usize = 256;
const IMAGE_EFFECT_API: &str = "OfxImageEffectPluginAPI";

/// One exact image-effect plugin header exported from an installed OFX binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFxPluginDescriptor {
    /// Globally unique plugin identifier supplied by the native binary.
    pub identifier: String,
    /// Image-effect API version; the current specified version is one.
    pub api_version: i32,
    /// Breaking plugin version component.
    pub version_major: u32,
    /// Compatible plugin version component.
    pub version_minor: u32,
}

/// Byte identity and exported OFX image-effect headers from one binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFxBinaryInspection {
    /// Canonical native binary selected for inspection.
    pub binary_path: PathBuf,
    /// Complete SHA-256 of that binary at the parent-side validation boundary.
    pub binary_sha256: String,
    /// Image-effect API v1 descriptors; no render contract is implied.
    pub plugins: Vec<OpenFxPluginDescriptor>,
}

/// Failure to inspect a native OFX binary without trusting its process.
#[derive(Debug, thiserror::Error)]
pub enum OpenFxDiscoveryError {
    /// Caller request or returned metadata violates the discovery contract.
    #[error("invalid OpenFX discovery contract: {0}")]
    Invalid(String),
    /// Native binary or helper could not be used.
    #[error("OpenFX discovery unavailable: {0}")]
    Unavailable(String),
    /// Native discovery exceeded its supervised deadline.
    #[error("OpenFX discovery worker exceeded its deadline")]
    DeadlineExceeded,
    /// Child exited without a valid response, including native crashes.
    #[error("OpenFX discovery worker failed: {0}")]
    WorkerFailed(String),
    /// The catalog owner closed before metadata publication.
    #[error("OpenFX discovery was canceled")]
    Canceled,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryRequest {
    binary_path: PathBuf,
    binary_sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DiscoveryResponse {
    Success {
        plugins: Vec<OpenFxPluginDescriptor>,
    },
    Rejected {
        reason: String,
    },
}

/// Inspect plugin headers in a deadline-bound child without loading the binary
/// into the editor process. No descriptor is admitted for rendering by this API.
pub fn inspect_openfx_binary(
    helper_executable: &Path,
    binary_path: &Path,
) -> Result<OpenFxBinaryInspection, OpenFxDiscoveryError> {
    inspect_openfx_binary_with_checkpoint(helper_executable, binary_path, &mut || Ok(()))
}

/// Inspect a binary while observing its owning catalog's cancellation authority.
pub fn inspect_openfx_binary_with_checkpoint(
    helper_executable: &Path,
    binary_path: &Path,
    checkpoint: &mut impl FnMut() -> Result<(), OpenFxDiscoveryError>,
) -> Result<OpenFxBinaryInspection, OpenFxDiscoveryError> {
    checkpoint()?;
    if !helper_executable.is_absolute() || !helper_executable.is_file() {
        return Err(invalid("OpenFX helper must be an absolute regular file"));
    }
    if !binary_path.is_absolute() {
        return Err(invalid("OpenFX binary path must be absolute"));
    }
    let binary_path = resolve_openfx_binary(binary_path)?;
    let before = sha256_file_with_checkpoint(&binary_path, checkpoint)?;
    let directory = tempfile::tempdir()
        .map_err(|error| unavailable(format!("discovery staging failed: {error}")))?;
    let request_path = directory.path().join("request.json");
    let response_path = directory.path().join("response.json");
    let request = serde_json::to_vec(&DiscoveryRequest {
        binary_path: binary_path.clone(),
        binary_sha256: before.clone(),
    })
    .map_err(|error| invalid(format!("request encoding failed: {error}")))?;
    if request.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(invalid("OpenFX discovery request exceeds the size limit"));
    }
    fs::write(&request_path, request)
        .map_err(|error| unavailable(format!("request write failed: {error}")))?;
    checkpoint()?;
    let mut child = Command::new(helper_executable)
        .arg(OPENFX_DISCOVERY_WORKER_ARGUMENT)
        .env(REQUEST_ENV, &request_path)
        .env(RESPONSE_ENV, &response_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unavailable(format!("worker start failed: {error}")))?;
    let deadline = Instant::now() + DEADLINE;
    let status = wait_for_child(&mut child, deadline, checkpoint).map_err(|error| match error {
        ChildWaitError::Checkpoint(error) => error,
        ChildWaitError::Deadline => OpenFxDiscoveryError::DeadlineExceeded,
        ChildWaitError::Observation(error) => {
            unavailable(format!("worker observation failed: {error}"))
        }
        ChildWaitError::Settlement(error) => {
            tracing::error!(pid = child.id(), %error, "OpenFX discovery child settlement failed");
            unavailable(format!("worker settlement failed: {error}"))
        }
    })?;
    if !status.success() {
        return Err(OpenFxDiscoveryError::WorkerFailed(status.to_string()));
    }
    checkpoint()?;
    let bytes = read_bounded(&response_path)?;
    let response: DiscoveryResponse = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("worker response is malformed: {error}")))?;
    if sha256_file_with_checkpoint(&binary_path, checkpoint)? != before {
        return Err(unavailable("binary changed during OpenFX discovery"));
    }
    let mut plugins = match response {
        DiscoveryResponse::Success { plugins } => plugins,
        DiscoveryResponse::Rejected { reason } => return Err(unavailable(reason)),
    };
    if plugins.len() > MAX_PLUGINS as usize {
        return Err(invalid("OpenFX descriptor count exceeds the limit"));
    }
    let mut unique = BTreeSet::new();
    for plugin in &plugins {
        if plugin.api_version != 1
            || !valid_identifier(&plugin.identifier)
            || !unique.insert((
                plugin.identifier.clone(),
                plugin.version_major,
                plugin.version_minor,
            ))
        {
            return Err(invalid(
                "OpenFX image-effect descriptor is invalid or duplicated",
            ));
        }
    }
    plugins.sort_by(|left, right| {
        left.identifier
            .cmp(&right.identifier)
            .then(left.version_major.cmp(&right.version_major))
            .then(left.version_minor.cmp(&right.version_minor))
    });
    checkpoint()?;
    Ok(OpenFxBinaryInspection { binary_path, binary_sha256: before, plugins })
}

pub(crate) fn resolve_openfx_binary(selection: &Path) -> Result<PathBuf, OpenFxDiscoveryError> {
    let selection = fs::canonicalize(selection)
        .map_err(|error| unavailable(format!("binary path cannot be resolved: {error}")))?;
    if selection.is_file() {
        return Ok(selection);
    }
    if !selection.is_dir()
        || !selection
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.to_ascii_lowercase().ends_with(".ofx.bundle"))
    {
        return Err(invalid(
            "OpenFX selection must be a binary or .ofx.bundle directory",
        ));
    }
    let architecture = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "Win64",
        ("windows", "x86") => "Win32",
        ("linux", "x86_64") => "Linux-x86-64",
        ("linux", "x86") => "Linux-x86",
        ("macos", _) => "MacOS",
        _ => return Err(invalid("OpenFX bundle architecture is unsupported")),
    };
    let native_dir = selection.join("Contents").join(architecture);
    let mut binary = None;
    for entry in fs::read_dir(&native_dir)
        .map_err(|error| unavailable(format!("OpenFX bundle has no native directory: {error}")))?
    {
        let entry =
            entry.map_err(|error| unavailable(format!("OpenFX bundle entry failed: {error}")))?;
        if !entry
            .file_type()
            .map_err(|error| unavailable(format!("OpenFX bundle entry type failed: {error}")))?
            .is_file()
            || !entry
                .path()
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("ofx"))
        {
            continue;
        }
        if binary.replace(entry.path()).is_some() {
            return Err(invalid("OpenFX bundle has multiple native binaries"));
        }
    }
    let binary =
        binary.ok_or_else(|| invalid("OpenFX bundle has no binary for this architecture"))?;
    let binary = fs::canonicalize(binary).map_err(|error| {
        unavailable(format!("OpenFX bundle binary cannot be resolved: {error}"))
    })?;
    if !binary.starts_with(&selection) || !binary.is_file() {
        return Err(invalid(
            "OpenFX bundle binary escapes the selected directory",
        ));
    }
    Ok(binary)
}

/// Execute the native-only discovery mode before graphics and media startup.
pub fn run_openfx_discovery_worker() -> Result<(), OpenFxDiscoveryError> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    if args.next().as_deref() != Some(OsStr::new(OPENFX_DISCOVERY_WORKER_ARGUMENT))
        || args.next().is_some()
    {
        return Err(invalid("unexpected OpenFX discovery worker arguments"));
    }
    let request_path = std::env::var_os(REQUEST_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("OpenFX discovery request path is missing"))?;
    let response_path = std::env::var_os(RESPONSE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("OpenFX discovery response path is missing"))?;
    let request: DiscoveryRequest = serde_json::from_slice(&read_bounded(&request_path)?)
        .map_err(|error| invalid(format!("OpenFX request is malformed: {error}")))?;
    let response = match inspect_loaded_binary(&request) {
        Ok(plugins) => DiscoveryResponse::Success { plugins },
        Err(error) => DiscoveryResponse::Rejected { reason: error.to_string() },
    };
    let bytes = serde_json::to_vec(&response)
        .map_err(|error| invalid(format!("OpenFX response encoding failed: {error}")))?;
    if bytes.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(invalid("OpenFX response exceeds the size limit"));
    }
    fs::write(response_path, bytes)
        .map_err(|error| unavailable(format!("OpenFX response write failed: {error}")))
}

unsafe extern "C" {
    fn mondrian_openfx_discovery_host_create(
        owner: *mut *mut c_void,
        handle: *mut *const c_void,
    ) -> c_int;
    fn mondrian_openfx_discovery_host_destroy(owner: *mut c_void);
}

struct NativeDiscoveryHost {
    owner: std::ptr::NonNull<c_void>,
    handle: std::ptr::NonNull<c_void>,
}

impl NativeDiscoveryHost {
    fn create() -> Result<Self, OpenFxDiscoveryError> {
        let mut owner = std::ptr::null_mut();
        let mut handle = std::ptr::null();
        // SAFETY: Writable output pointers stay live; the native host never runs
        // installed code and returns one owned descriptor or clears both outputs.
        let status = unsafe { mondrian_openfx_discovery_host_create(&mut owner, &mut handle) };
        if status != 0 {
            return Err(unavailable(format!(
                "native discovery host failed ({status})"
            )));
        }
        let owner = std::ptr::NonNull::new(owner)
            .ok_or_else(|| unavailable("native discovery host has no owner"))?;
        let Some(handle) = std::ptr::NonNull::new(handle.cast_mut()) else {
            // SAFETY: The successful native call transferred this exact owner.
            unsafe { mondrian_openfx_discovery_host_destroy(owner.as_ptr()) };
            return Err(unavailable("native discovery host has no descriptor"));
        };
        Ok(Self { owner, handle })
    }
}

impl Drop for NativeDiscoveryHost {
    fn drop(&mut self) {
        // SAFETY: The guard exclusively owns this exact allocation and is dropped
        // after the selected library, including on every failed bootstrap path.
        unsafe { mondrian_openfx_discovery_host_destroy(self.owner.as_ptr()) };
    }
}

fn inspect_loaded_binary(
    request: &DiscoveryRequest,
) -> Result<Vec<OpenFxPluginDescriptor>, OpenFxDiscoveryError> {
    let path = fs::canonicalize(&request.binary_path)
        .map_err(|error| unavailable(format!("binary cannot be resolved: {error}")))?;
    if path != request.binary_path
        || !path.is_file()
        || sha256_file(&path)? != request.binary_sha256
    {
        return Err(invalid(
            "OpenFX worker received a changed or noncanonical binary",
        ));
    }
    // SAFETY: The selected binary is loaded only in this short-lived worker.
    // All exported pointers remain borrowed while `library` is alive. A bad
    // plugin can still crash this process; the parent supervises that failure.
    let plugins = unsafe {
        let host = NativeDiscoveryHost::create()?;
        let library = Library::new(&path)
            .map_err(|error| unavailable(format!("OpenFX binary load failed: {error}")))?;
        let count = library
            .get::<unsafe extern "C" fn() -> c_int>(b"OfxGetNumberOfPlugins\0")
            .map_err(|error| invalid(format!("OpenFX count export is missing: {error}")))?;
        let get_plugin = library
            .get::<unsafe extern "C" fn(c_int) -> *const OfxPlugin>(b"OfxGetPlugin\0")
            .map_err(|error| invalid(format!("OpenFX descriptor export is missing: {error}")))?;
        // The optional export precedes all enumeration calls. A not-for-host
        // response is a successful empty discovery, not a native loading fault.
        let admitted =
            match library.get::<unsafe extern "C" fn(*const c_void) -> c_int>(b"OfxSetHost\0") {
                Ok(bootstrap) => match bootstrap(host.handle.as_ptr().cast_const()) {
                    0 => true,
                    1 => false,
                    status => return Err(invalid(format!("OpenFX bootstrap failed ({status})"))),
                },
                Err(_) => true,
            };
        let count = if admitted { count() } else { 0 };
        if !(0..=MAX_PLUGINS).contains(&count) {
            return Err(invalid("OpenFX plugin count is outside the admitted range"));
        }
        let mut plugins = Vec::new();
        for index in 0..count {
            let pointer = get_plugin(index);
            if pointer.is_null() {
                return Err(invalid(format!("OpenFX plugin {index} has no descriptor")));
            }
            let header = &*pointer;
            if header.api_version != 1 || !is_image_effect_api(header.plugin_api) {
                continue;
            }
            let identifier = read_ascii(header.plugin_identifier, MAX_IDENTIFIER_BYTES)?;
            if !valid_identifier(&identifier)
                || header.set_host.is_none()
                || header.main_entry.is_none()
            {
                return Err(invalid(format!(
                    "OpenFX image-effect plugin {index} has an invalid header"
                )));
            }
            plugins.push(OpenFxPluginDescriptor {
                identifier,
                api_version: header.api_version,
                version_major: header.version_major,
                version_minor: header.version_minor,
            });
        }
        plugins
    };
    if sha256_file(&path)? != request.binary_sha256 {
        return Err(unavailable("OpenFX binary changed while loaded"));
    }
    Ok(plugins)
}

#[repr(C)]
struct OfxPlugin {
    plugin_api: *const c_char,
    api_version: c_int,
    plugin_identifier: *const c_char,
    version_major: c_uint,
    version_minor: c_uint,
    set_host: Option<unsafe extern "C" fn(*mut c_void)>,
    main_entry: Option<
        unsafe extern "C" fn(*const c_char, *const c_void, *mut c_void, *mut c_void) -> c_int,
    >,
}

// Compare only the one admitted API, including its terminator. Unknown APIs
// have no product string-size/ASCII requirement and need no owned allocation.
unsafe fn is_image_effect_api(pointer: *const c_char) -> bool {
    if pointer.is_null() {
        return false;
    }
    for (index, expected) in IMAGE_EFFECT_API.bytes().chain(std::iter::once(0)).enumerate() {
        // SAFETY: A legal header contains a terminated C string. Stop at the
        // first differing byte (including an early NUL), so a short string is
        // never read beyond its terminator. Malformed plugins remain supervised.
        if unsafe { *pointer.add(index) } as u8 != expected {
            return false;
        }
    }
    true
}

unsafe fn read_ascii(
    pointer: *const c_char,
    capacity: usize,
) -> Result<String, OpenFxDiscoveryError> {
    if pointer.is_null() {
        return Err(invalid("OpenFX descriptor string pointer is null"));
    }
    let mut bytes = Vec::new();
    for index in 0..capacity {
        // SAFETY: A plugin-provided pointer may be malformed, but this read is
        // confined to the supervised worker and capped at `capacity` bytes.
        let byte = unsafe { *pointer.add(index) } as u8;
        if byte == 0 {
            return String::from_utf8(bytes)
                .map_err(|error| invalid(format!("OpenFX descriptor is not ASCII: {error}")));
        }
        if !(0x20..=0x7e).contains(&byte) {
            return Err(invalid(
                "OpenFX descriptor contains a non-ASCII or control byte",
            ));
        }
        bytes.push(byte);
    }
    Err(invalid("OpenFX descriptor string exceeds its size limit"))
}

fn valid_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() < MAX_IDENTIFIER_BYTES
        && identifier.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, OpenFxDiscoveryError> {
    sha256_file_with_checkpoint(path, &mut || Ok(()))
}

pub(crate) fn sha256_file_with_checkpoint<E: From<OpenFxDiscoveryError>>(
    path: &Path,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<String, E> {
    checkpoint()?;
    let mut file = File::open(path)
        .map_err(|error| unavailable(format!("OpenFX binary cannot be read: {error}")))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        checkpoint()?;
        let count = file
            .read(&mut buffer)
            .map_err(|error| unavailable(format!("OpenFX binary read failed: {error}")))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>, OpenFxDiscoveryError> {
    let metadata = fs::metadata(path)
        .map_err(|error| unavailable(format!("OpenFX worker message is unavailable: {error}")))?;
    if !metadata.is_file() || metadata.len() > MAX_MESSAGE_BYTES {
        return Err(invalid("OpenFX worker message exceeds its size limit"));
    }
    let bytes = fs::read(path)
        .map_err(|error| unavailable(format!("OpenFX worker message cannot be read: {error}")))?;
    if bytes.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(invalid(
            "OpenFX worker message changed beyond its size limit",
        ));
    }
    Ok(bytes)
}

fn invalid(reason: impl Into<String>) -> OpenFxDiscoveryError {
    OpenFxDiscoveryError::Invalid(reason.into())
}

fn unavailable(reason: impl Into<String>) -> OpenFxDiscoveryError {
    OpenFxDiscoveryError::Unavailable(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canceled_discovery_never_resolves_or_spawns_an_invalid_selection() {
        assert!(matches!(
            inspect_openfx_binary_with_checkpoint(
                Path::new("invalid-helper"),
                Path::new("invalid-plugin"),
                &mut || Err(OpenFxDiscoveryError::Canceled),
            ),
            Err(OpenFxDiscoveryError::Canceled)
        ));
    }

    #[test]
    fn binary_identity_hash_observes_cancellation_between_read_chunks() {
        let directory = tempfile::tempdir().expect("staging");
        let path = directory.path().join("binary.ofx");
        fs::write(&path, vec![0_u8; 128 * 1024]).expect("fixture bytes");
        let mut checkpoints = 0;
        let result = sha256_file_with_checkpoint(&path, &mut || {
            checkpoints += 1;
            if checkpoints == 3 {
                Err(OpenFxDiscoveryError::Canceled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(OpenFxDiscoveryError::Canceled)));
    }

    #[test]
    fn descriptor_contract_rejects_controls_and_duplicates() {
        assert!(valid_identifier("org.openfx.Basic"));
        assert!(!valid_identifier("bad identifier"));
        assert!(!valid_identifier("bad\nidentifier"));
        assert!(!valid_identifier(""));
    }

    #[test]
    fn native_loading_is_not_attempted_for_invalid_parent_paths() {
        let helper = Path::new("relative-helper");
        assert!(matches!(
            inspect_openfx_binary(helper, Path::new("relative.ofx")),
            Err(OpenFxDiscoveryError::Invalid(_))
        ));
    }

    #[test]
    fn bundle_resolution_requires_one_native_binary_for_this_architecture() {
        let root = tempfile::tempdir().expect("bundle fixture");
        let bundle = root.path().join("Basic.ofx.bundle");
        let architecture = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => "Win64",
            ("windows", "x86") => "Win32",
            ("linux", "x86_64") => "Linux-x86-64",
            ("linux", "x86") => "Linux-x86",
            ("macos", _) => "MacOS",
            _ => return,
        };
        let native = bundle.join("Contents").join(architecture);
        fs::create_dir_all(&native).expect("native directory");
        let first = native.join("Basic.ofx");
        fs::write(&first, b"fixture").expect("first binary");
        assert_eq!(
            resolve_openfx_binary(&bundle).expect("one binary"),
            fs::canonicalize(&first).expect("canonical")
        );
        fs::write(native.join("Other.ofx"), b"fixture").expect("second binary");
        assert!(matches!(
            resolve_openfx_binary(&bundle),
            Err(OpenFxDiscoveryError::Invalid(_))
        ));
    }
    #[test]
    #[ignore = "requires the owned SessionProbe native bundle"]
    fn native_discovery_skips_long_unknown_apis_before_descriptor_validation() {
        const TEST: &str = "openfx_adapter::tests::native_discovery_skips_long_unknown_apis_before_descriptor_validation";
        if !crate::openfx_host::shared_frame_tests::qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "mixed_api_long")],
        ) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned SessionProbe binary"),
        ))
        .expect("probe path");
        let request = DiscoveryRequest {
            binary_path: path.clone(),
            binary_sha256: sha256_file(&path).expect("probe fingerprint"),
        };
        for _ in 0..3 {
            let plugins = inspect_loaded_binary(&request)
                .expect("skip unknown long API without applying known-API metadata bounds");
            assert_eq!(plugins.len(), 1);
            assert_eq!(plugins[0].identifier, "uk.co.thefoundry.BasicGainPlugin");
            assert_eq!(plugins[0].api_version, 1);
        }
    }

    #[test]
    #[ignore = "requires the owned SessionProbe native bundle"]
    fn native_discovery_bootstrap_owns_image_memory_and_releases_host_binding() {
        const TEST: &str = "openfx_adapter::tests::native_discovery_bootstrap_owns_image_memory_and_releases_host_binding";
        if !crate::openfx_host::shared_frame_tests::qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "bootstrap_memory")],
        ) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned SessionProbe binary"),
        ))
        .expect("probe path");
        let request = DiscoveryRequest {
            binary_path: path.clone(),
            binary_sha256: sha256_file(&path).expect("probe fingerprint"),
        };
        for _ in 0..3 {
            let plugins = inspect_loaded_binary(&request)
                .expect("host-owned image memory must work during early bootstrap");
            assert_eq!(plugins.len(), 1);
            assert_eq!(plugins[0].identifier, "uk.co.thefoundry.BasicGainPlugin");
        }
        // Sequential owners must fully release the native singleton binding.
        // The test uses the existing constructor, without exporting a test-only
        // global accessor or introducing another native bootstrap owner path.
        let first = NativeDiscoveryHost::create().expect("fresh owner after repeated bootstrap");
        drop(first);
        let second = NativeDiscoveryHost::create().expect("binding released before next owner");
        drop(second);
    }

    #[test]
    #[ignore = "requires the owned SessionProbe native bundle"]
    fn native_discovery_refuses_busy_owners_without_waiting_or_switching_host_callbacks() {
        const TEST: &str = "openfx_adapter::tests::native_discovery_refuses_busy_owners_without_waiting_or_switching_host_callbacks";
        if !crate::openfx_host::shared_frame_tests::qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "bootstrap_memory")],
        ) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned SessionProbe binary"),
        ))
        .expect("probe path");
        let first = NativeDiscoveryHost::create().expect("first host owner");
        assert!(
            matches!(NativeDiscoveryHost::create(),
            Err(OpenFxDiscoveryError::Unavailable(reason)) if reason.ends_with("(9)")),
            "same-thread second owner must return native HostBusy immediately"
        );
        let other = std::thread::spawn(|| {
            matches!(NativeDiscoveryHost::create(),
            Err(OpenFxDiscoveryError::Unavailable(reason)) if reason.ends_with("(9)"))
        });
        assert!(
            other.join().expect("second owner thread"),
            "cross-thread owner must return HostBusy, without waiting for first drop"
        );
        crate::openfx_host::shared_frame_tests::native_canonical_geometry_overflow_refuses_before_binary_loading();
        // The first owner's live suite must remain usable after both refused
        // attempts. The fixture performs allocation/lock/write/read/free through
        // the exposed suite, rather than inspecting a vendor global pointer.
        // SAFETY: Library and its export remain live until after bootstrap; the
        // first owner remains live until the library has been unloaded.
        unsafe {
            let library = Library::new(path).expect("bootstrap fixture library");
            let bootstrap = library
                .get::<unsafe extern "C" fn(*const c_void) -> c_int>(b"OfxSetHost\0")
                .expect("owned optional bootstrap");
            assert_eq!(
                bootstrap(first.handle.as_ptr().cast_const()),
                0,
                "refused owners must not replace the first host's image-memory binding"
            );
        }
        drop(first);
        let next = NativeDiscoveryHost::create().expect("owner after first complete teardown");
        drop(next);
    }
}
