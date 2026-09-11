//! Process entry policy, independent of graphics resource ownership.

/// Failure to establish the graphics loader policy before opening resources.
#[derive(Debug, thiserror::Error)]
pub enum GraphicsProcessBootstrapError {
    /// An inherited setting explicitly contradicts the Linux loader policy.
    #[error("Linux graphics requires VK_LOADER_DISABLE_DYNAMIC_LIBRARY_UNLOADING=1; remove the conflicting value {value:?}")]
    ConflictingLoaderPolicy {
        /// The conflicting value, retained without lossy Unicode conversion.
        value: std::ffi::OsString,
    },
    /// The same process image could not be reexecuted with its loader policy.
    #[error("could not establish the Linux graphics loader policy before startup: {source}")]
    Reexecute {
        /// Operating-system failure returned by exec.
        #[source]
        source: std::io::Error,
    },
}

/// Establishes the desktop graphics process policy before starting any owner.
///
/// Call this from process `main`, before threads, tracing, media workers, or
/// graphics initialization. On Linux this reexecutes the running image once
/// with the Vulkan loader's dynamic-library unloading disabled. It preserves
/// PID, arguments (including non-Unicode argv[0]), environment, working directory
/// and standard streams; it never mutates the current process environment or
/// leaves a supervisor child behind. An explicit conflicting value fails closed.
/// Other platforms do nothing.
///
/// Vulkan loader 1.3.259 or later is required to honor this policy. Driver code
/// libraries remain resident until process exit; Vulkan instances, devices,
/// queues and surfaces still have their ordinary consuming owners. This is a
/// workaround for concurrent driver unloading, not device closure or physical
/// qualification evidence. Library consumers must establish the same policy in
/// their own process entrypoint; this function must not be called by an already
/// running graphics session or from an embedding application's worker thread.
pub fn prepare_graphics_process() -> Result<(), GraphicsProcessBootstrapError> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::OsStr;
        use std::os::unix::process::CommandExt;

        const POLICY: &str = "VK_LOADER_DISABLE_DYNAMIC_LIBRARY_UNLOADING";
        match std::env::var_os(POLICY) {
            Some(value) if value == OsStr::new("1") => return Ok(()),
            Some(value) => {
                return Err(GraphicsProcessBootstrapError::ConflictingLoaderPolicy { value });
            }
            None => {}
        }
        let mut arguments = std::env::args_os();
        let mut replacement = std::process::Command::new("/proc/self/exe");
        if let Some(arg0) = arguments.next() {
            replacement.arg0(arg0);
        }
        let source = replacement.args(arguments).env(POLICY, "1").exec();
        Err(GraphicsProcessBootstrapError::Reexecute { source })
    }
    #[cfg(not(target_os = "linux"))]
    Ok(())
}
