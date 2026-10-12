//! Process entry policy, independent of graphics resource ownership.

/// Failure to establish platform process policy before opening graphics resources.
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
    /// The native application role could not be observed.
    #[cfg(target_os = "macos")]
    #[error("could not observe the macOS graphics application role: {source}")]
    MacApplicationRoleObservation {
        #[source]
        source: crate::PlaybackThreadSchedulingError,
    },
    /// The native default application role was rejected before owner startup.
    #[cfg(target_os = "macos")]
    #[error("task_policy_set(TASK_DEFAULT_APPLICATION) failed: native result={native_result}")]
    MacApplicationRoleAdmission { native_result: i32 },
    /// Readback did not establish the requested default application role.
    #[cfg(target_os = "macos")]
    #[error("macOS graphics application role readback was {observed_role}, expected 7")]
    MacApplicationRoleReadback { observed_role: i32 },
}

/// Establishes the desktop graphics process policy before starting any owner.
///
/// Call this from process `main`, before threads, tracing, media workers, or
/// graphics initialization. On Linux this reexecutes the running image once
/// with the Vulkan loader's dynamic-library unloading disabled. It preserves
/// PID, arguments (including non-Unicode `argv[0]`), environment, working directory
/// and standard streams; it never mutates the current process environment or
/// leaves a supervisor child behind. An explicit conflicting value fails closed.
/// On macOS an unspecified application role becomes TASK_DEFAULT_APPLICATION.
/// This establishes the application's process policy so pthread QoS is not
/// capped by the unspecified role. It does not claim foreground visibility;
/// AppKit continues to own subsequent foreground/background transitions. Any
/// already assigned native role is preserved. Admission is process-lifetime,
/// and rejected policy or mismatched readback fails before owners start.
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
    #[cfg(target_os = "macos")]
    {
        prepare_macos_graphics_process()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Ok(())
}

#[cfg(target_os = "macos")]
fn prepare_macos_graphics_process() -> Result<(), GraphicsProcessBootstrapError> {
    const TASK_CATEGORY_POLICY: u32 = 1;
    const TASK_DEFAULT_APPLICATION: i32 = 7;
    let observe_role = || {
        crate::observe_macos_playback_task_role().map_err(|source| {
            GraphicsProcessBootstrapError::MacApplicationRoleObservation { source }
        })
    };
    if observe_role()? != 0 {
        return Ok(());
    }
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn task_policy_set(task: u32, flavor: u32, policy: *const i32, count: u32) -> i32;
    }
    // Public mach/task_policy.h: category policy contains one integer role.
    // SAFETY: Only this process's task is changed, at its executable entry.
    // The call reads one live integer and retains no pointer or Mach right.
    let native_result = unsafe {
        task_policy_set(
            mach_task_self_,
            TASK_CATEGORY_POLICY,
            &TASK_DEFAULT_APPLICATION,
            1,
        )
    };
    if native_result != 0 {
        return Err(GraphicsProcessBootstrapError::MacApplicationRoleAdmission { native_result });
    }
    let observed_role = observe_role()?;
    if observed_role != TASK_DEFAULT_APPLICATION {
        return Err(GraphicsProcessBootstrapError::MacApplicationRoleReadback { observed_role });
    }
    Ok(())
}
