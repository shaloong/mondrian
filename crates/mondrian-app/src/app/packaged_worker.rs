//! Discovery of the packaged product executable used by hidden worker modes.
//!
//! Preview demux and media probing are two real Adapters at this seam. They
//! share executable discovery while retaining independent protocols,
//! lifecycles, cancellation, and evidence.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PackagedWorkerDiscoveryError {
    #[error("configured {purpose} worker does not exist: {path}")]
    ConfiguredPathMissing {
        purpose: &'static str,
        path: PathBuf,
    },
    #[error("cannot resolve the current product executable for {purpose}: {source}")]
    CurrentExecutable {
        purpose: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "packaged {purpose} worker is unavailable beside current executable {current_executable}"
    )]
    ProductExecutableMissing {
        purpose: &'static str,
        current_executable: PathBuf,
    },
}

/// Maximum product time admitted for one physical media Probe Helper.
pub(crate) const MEDIA_PROBE_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn discover_preview_demux_worker() -> Result<PathBuf, PackagedWorkerDiscoveryError> {
    discover_required_packaged_app_worker("MONDRIAN_PREVIEW_DEMUX_WORKER_PATH", "Preview demux")
}

pub(crate) fn discover_media_probe_worker() -> Option<PathBuf> {
    discover_packaged_app_worker("MONDRIAN_MEDIA_PROBE_WORKER_PATH", "media probe")
}

fn discover_packaged_app_worker(override_environment: &str, purpose: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(override_environment) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
        tracing::warn!(
            path = %path.display(),
            purpose,
            "configured packaged worker does not exist; continuing product executable discovery"
        );
    }

    let current = std::env::current_exe().ok()?;
    if current.file_stem().is_some_and(|name| name.eq_ignore_ascii_case("mondrian")) {
        return Some(current);
    }

    // Cargo tests and validation binaries live either beside the product
    // executable or in target/{profile}/deps. The product binary implements
    // every hidden worker mode so its FFmpeg ABI and private runtime closure
    // remain identical to the App process.
    let directory = current.parent()?;
    let profile_directory = if directory.file_name().is_some_and(|name| name == "deps") {
        directory.parent()?
    } else {
        directory
    };
    let candidate = profile_directory.join(format!("mondrian{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

fn discover_required_packaged_app_worker(
    override_environment: &str,
    purpose: &'static str,
) -> Result<PathBuf, PackagedWorkerDiscoveryError> {
    if let Some(path) = std::env::var_os(override_environment) {
        let path = PathBuf::from(path);
        return path
            .is_file()
            .then_some(path.clone())
            .ok_or(PackagedWorkerDiscoveryError::ConfiguredPathMissing { purpose, path });
    }

    let current = std::env::current_exe()
        .map_err(|source| PackagedWorkerDiscoveryError::CurrentExecutable { purpose, source })?;
    if current.file_stem().is_some_and(|name| name.eq_ignore_ascii_case("mondrian")) {
        return Ok(current);
    }

    let Some(directory) = current.parent() else {
        return Err(PackagedWorkerDiscoveryError::ProductExecutableMissing {
            purpose,
            current_executable: current,
        });
    };
    let profile_directory = if directory.file_name().is_some_and(|name| name == "deps") {
        directory.parent().unwrap_or(directory)
    } else {
        directory
    };
    let candidate = profile_directory.join(format!("mondrian{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate).ok_or(
        PackagedWorkerDiscoveryError::ProductExecutableMissing {
            purpose,
            current_executable: current,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_override_does_not_become_worker_authority() {
        let variable = format!("MONDRIAN_TEST_MISSING_WORKER_{}", std::process::id());
        // SAFETY: this unique variable is not observed by another test or any
        // production module.
        unsafe { std::env::set_var(&variable, "definitely/missing/mondrian-worker") };
        let worker = discover_packaged_app_worker(&variable, "test");
        // Discovery may still find an actually built product sibling. The
        // configured missing path itself must never be returned as authority.
        assert!(worker
            .as_deref()
            .is_none_or(|path| path != std::path::Path::new("definitely/missing/mondrian-worker")));
        // SAFETY: restore the unique process environment key after the test.
        unsafe { std::env::remove_var(variable) };
    }

    #[test]
    fn required_worker_rejects_a_missing_configured_path_without_fallback() {
        let variable = format!(
            "MONDRIAN_TEST_REQUIRED_MISSING_WORKER_{}",
            std::process::id()
        );
        // SAFETY: this unique variable is not observed by another test or any
        // production module.
        unsafe { std::env::set_var(&variable, "definitely/missing/mondrian-worker") };
        let result = discover_required_packaged_app_worker(&variable, "test worker");
        // SAFETY: restore the unique process environment key after the test.
        unsafe { std::env::remove_var(variable) };

        assert!(matches!(
            result,
            Err(PackagedWorkerDiscoveryError::ConfiguredPathMissing { purpose: "test worker", .. })
        ));
    }
}
