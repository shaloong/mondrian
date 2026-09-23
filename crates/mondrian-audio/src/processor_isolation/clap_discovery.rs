//! Bounded out-of-process discovery of installed CLAP descriptors.

use crate::AudioProcessorHostError;
use clack_host::prelude::PluginEntry;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Hidden application mode used to inspect one user-installed CLAP binary.
pub const CLAP_DISCOVERY_WORKER_ARGUMENT: &str = "--internal-clap-discovery-v1";
const REQUEST_ENV: &str = "MONDRIAN_INTERNAL_CLAP_DISCOVERY_REQUEST";
const RESPONSE_ENV: &str = "MONDRIAN_INTERNAL_CLAP_DISCOVERY_RESPONSE";
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);
const MAX_DISCOVERY_BYTES: u64 = 256 * 1024;
const MAX_DESCRIPTORS: usize = 4096;

/// One discovered CLAP identity for display and persistent definition matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClapPluginDescriptor {
    /// Stable CLAP plugin ID; installed paths are not persisted as identity.
    pub plugin_id: String,
    /// User-facing plugin name supplied by the installed library.
    pub name: String,
    /// Optional vendor label supplied by the installed library.
    pub vendor: Option<String>,
    /// Optional vendor version supplied by the installed library.
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryRequest {
    library_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryResponse {
    descriptors: Vec<ClapPluginDescriptor>,
}

/// Discover descriptors without loading the native module in the editor process.
///
/// A child failure, hang, malformed response, or excessive metadata makes this
/// one library unavailable. The caller may continue scanning other libraries.
pub fn scan_clap_library_descriptors(
    helper_executable: &Path,
    library_path: &Path,
) -> Result<Vec<ClapPluginDescriptor>, AudioProcessorHostError> {
    if !helper_executable.is_absolute() || !library_path.is_absolute() {
        return Err(invalid(
            "CLAP discovery requires absolute helper and library paths",
        ));
    }
    let library_path = fs::canonicalize(library_path)
        .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
    if !library_path.is_file() {
        return Err(unavailable("CLAP library path is not a regular file"));
    }
    let temp = tempfile::tempdir()
        .map_err(|error| unavailable(format!("CLAP discovery staging failed: {error}")))?;
    let request_path = temp.path().join("request.json");
    let response_path = temp.path().join("response.json");
    let request = serde_json::to_vec(&DiscoveryRequest { library_path })
        .map_err(|error| invalid(format!("CLAP discovery request failed: {error}")))?;
    fs::write(&request_path, request)
        .map_err(|error| unavailable(format!("CLAP discovery request write failed: {error}")))?;
    let mut child = Command::new(helper_executable)
        .arg(CLAP_DISCOVERY_WORKER_ARGUMENT)
        .env(REQUEST_ENV, &request_path)
        .env(RESPONSE_ENV, &response_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unavailable(format!("CLAP discovery worker failed to start: {error}")))?;
    let deadline = Instant::now() + DISCOVERY_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                return Err(unavailable(format!(
                    "CLAP discovery worker failed with status {status}"
                )));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AudioProcessorHostError::WorkerDeadlineExceeded {
                    operation: "CLAP discovery",
                });
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable(format!(
                    "CLAP discovery worker wait failed: {error}"
                )));
            }
        }
    }
    let response = read_bounded(&response_path, "response")?;
    let response: DiscoveryResponse = serde_json::from_slice(&response)
        .map_err(|error| invalid(format!("CLAP discovery response is invalid: {error}")))?;
    validate_descriptors(&response.descriptors)?;
    Ok(response.descriptors)
}

/// Execute the hidden discovery mode before any UI, GPU, or media initialization.
pub fn run_clap_discovery_worker() -> Result<(), AudioProcessorHostError> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    if args.next().as_deref() != Some(OsStr::new(CLAP_DISCOVERY_WORKER_ARGUMENT))
        || args.next().is_some()
    {
        return Err(invalid("invalid CLAP discovery worker arguments"));
    }
    let request_path = std::env::var_os(REQUEST_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("CLAP discovery request path is missing"))?;
    let response_path = std::env::var_os(RESPONSE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("CLAP discovery response path is missing"))?;
    let request = read_bounded(&request_path, "request")?;
    let request: DiscoveryRequest = serde_json::from_slice(&request)
        .map_err(|error| invalid(format!("CLAP discovery request is invalid: {error}")))?;
    if !request.library_path.is_absolute() {
        return Err(invalid("CLAP discovery library path must be absolute"));
    }
    let entry = unsafe { PluginEntry::load(request.library_path.as_os_str()) }
        .map_err(|error| unavailable(format!("CLAP library load failed: {error}")))?;
    let descriptors = list_loaded(&entry)?;
    let response = serde_json::to_vec(&DiscoveryResponse { descriptors })
        .map_err(|error| invalid(format!("CLAP discovery response encoding failed: {error}")))?;
    if response.len() as u64 > MAX_DISCOVERY_BYTES {
        return Err(invalid(
            "CLAP discovery response exceeds the metadata limit",
        ));
    }
    fs::write(response_path, response)
        .map_err(|error| unavailable(format!("CLAP discovery response write failed: {error}")))
}

fn list_loaded(entry: &PluginEntry) -> Result<Vec<ClapPluginDescriptor>, AudioProcessorHostError> {
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    let mut descriptors = Vec::new();
    for descriptor in factory.plugin_descriptors() {
        if descriptors.len() == MAX_DESCRIPTORS {
            return Err(invalid("CLAP library exceeds descriptor capacity"));
        }
        let id = descriptor
            .id()
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| invalid("CLAP descriptor has no valid UTF-8 ID"))?;
        let name = descriptor
            .name()
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| invalid("CLAP descriptor has no valid UTF-8 name"))?;
        descriptors.push(ClapPluginDescriptor {
            plugin_id: id.to_owned(),
            name: name.to_owned(),
            vendor: descriptor.vendor().and_then(|value| value.to_str().ok()).map(str::to_owned),
            version: descriptor.version().and_then(|value| value.to_str().ok()).map(str::to_owned),
        });
    }
    validate_descriptors(&descriptors)?;
    Ok(descriptors)
}

fn validate_descriptors(
    descriptors: &[ClapPluginDescriptor],
) -> Result<(), AudioProcessorHostError> {
    if descriptors.len() > MAX_DESCRIPTORS {
        return Err(invalid("CLAP library exceeds descriptor capacity"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for descriptor in descriptors {
        if descriptor.plugin_id.is_empty()
            || descriptor.plugin_id.len() > 256
            || descriptor.name.is_empty()
            || descriptor.name.len() > 256
            || descriptor.vendor.as_ref().is_some_and(|value| value.len() > 256)
            || descriptor.version.as_ref().is_some_and(|value| value.len() > 256)
            || !seen.insert(&descriptor.plugin_id)
        {
            return Err(invalid("CLAP descriptor identity or metadata is invalid"));
        }
    }
    Ok(())
}

fn read_bounded(path: &Path, name: &str) -> Result<Vec<u8>, AudioProcessorHostError> {
    let file = fs::File::open(path)
        .map_err(|error| unavailable(format!("CLAP discovery {name} open failed: {error}")))?;
    let mut bytes = Vec::new();
    file.take(MAX_DISCOVERY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unavailable(format!("CLAP discovery {name} read failed: {error}")))?;
    if bytes.len() as u64 > MAX_DISCOVERY_BYTES {
        return Err(invalid(format!(
            "CLAP discovery {name} exceeds the metadata limit"
        )));
    }
    Ok(bytes)
}

fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}

fn unavailable(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::Unavailable(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clack_plugin::prelude::{
        DefaultPluginFactory, HostMainThreadHandle, HostSharedHandle, Plugin, PluginDescriptor,
        PluginError, SinglePluginEntry,
    };

    struct DiscoveryFixture;

    impl Plugin for DiscoveryFixture {
        type AudioProcessor<'a> = ();
        type Shared<'a> = ();
        type MainThread<'a> = ();
    }

    impl DefaultPluginFactory for DiscoveryFixture {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new("org.mondrian.discovery-fixture", "Discovery Fixture")
                .with_vendor("Mondrian")
                .with_version("1.2.3")
        }

        fn new_shared(_host: HostSharedHandle<'_>) -> Result<(), PluginError> {
            Ok(())
        }

        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            _shared: &'a (),
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }

    #[test]
    fn discovers_clap_descriptor_from_abi_factory() {
        let entry =
            PluginEntry::load_from_clack::<SinglePluginEntry<DiscoveryFixture>>(c"/test/discovery")
                .expect("static discovery entry");
        let descriptors = list_loaded(&entry).expect("discover static CLAP entry");
        assert_eq!(
            descriptors,
            vec![ClapPluginDescriptor {
                plugin_id: "org.mondrian.discovery-fixture".to_owned(),
                name: "Discovery Fixture".to_owned(),
                vendor: Some("Mondrian".to_owned()),
                version: Some("1.2.3".to_owned()),
            }]
        );
    }

    #[test]
    fn rejects_duplicate_or_oversized_discovery_metadata() {
        let descriptor = ClapPluginDescriptor {
            plugin_id: "org.example.gain".to_owned(),
            name: "Gain".to_owned(),
            vendor: None,
            version: None,
        };
        assert!(validate_descriptors(&[descriptor.clone(), descriptor.clone()]).is_err());
        assert!(validate_descriptors(&[ClapPluginDescriptor {
            name: "x".repeat(257),
            ..descriptor
        }])
        .is_err());
    }

    #[test]
    fn bounded_reader_rejects_oversized_worker_response() {
        let temp = tempfile::tempdir().expect("temporary discovery directory");
        let response_path = temp.path().join("response.json");
        fs::write(&response_path, vec![b'x'; MAX_DISCOVERY_BYTES as usize + 1])
            .expect("write oversized response");
        assert!(matches!(
            read_bounded(&response_path, "response"),
            Err(AudioProcessorHostError::InvalidContract(_))
        ));
    }

    #[test]
    fn scanner_reports_child_failure_without_editor_crash() {
        let test_executable = std::env::current_exe().expect("test executable path");
        assert!(matches!(
            scan_clap_library_descriptors(&test_executable, &test_executable),
            Err(AudioProcessorHostError::Unavailable(_))
        ));
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn installed_clap_reference_is_discovered_across_process_boundary() {
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian-app executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let descriptors = scan_clap_library_descriptors(&helper, &plugin)
            .expect("discover installed CLAP reference plugin");
        assert!(descriptors.iter().any(|descriptor| {
            descriptor.plugin_id == "org.rust-audio.clack.gain"
                && descriptor.name == "Clack Gain Example"
        }));
    }
}
