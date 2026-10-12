//! Thread-affine native scheduling for the product playback coordinator.

/// Observable state of the current thread's product playback scheduling class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackThreadSchedulingStatus {
    /// No playback scheduling class is active.
    Inactive,
    /// The native multimedia playback class is active on this thread.
    Active,
    /// Playback remains valid under the portable scheduler contract because
    /// this residency could not or need not enter a native priority class.
    PortableFallback,
}

/// Failure to enter the native multimedia playback scheduling class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackThreadSchedulingError {
    operation: &'static str,
    detail: String,
}

impl PlaybackThreadSchedulingError {
    fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self { operation, detail: detail.into() }
    }
}

impl std::fmt::Display for PlaybackThreadSchedulingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} failed: {}", self.operation, self.detail)
    }
}

impl std::error::Error for PlaybackThreadSchedulingError {}

/// Read-only diagnostic of the calling macOS thread, not a playback clock.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
pub struct MacPlaybackThreadObservation {
    pub thread_id: u64,
    pub cpu_time_ns: u64,
    pub requested_qos_class: u32,
    pub relative_priority: i32,
    pub base_priority: i32,
    pub current_priority: i32,
    pub maximum_priority: i32,
    pub scheduling_policy: i32,
}

/// Read the calling process's public Mach application role without changing it.
///
/// A pthread QoS request can be limited by this process policy. The role alone
/// does not prove effective thread priority or bounded wake latency.
#[cfg(target_os = "macos")]
pub fn observe_macos_playback_task_role() -> Result<i32, PlaybackThreadSchedulingError> {
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn task_policy_get(
            task: u32,
            flavor: u32,
            policy: *mut i32,
            count: *mut u32,
            get_default: *mut i32,
        ) -> i32;
    }
    // Public mach/task_policy.h: TASK_CATEGORY_POLICY is one integer role.
    let mut role = 0;
    let mut count = 1;
    let mut get_default = 0;
    // SAFETY: The initialized self-task port is process-owned. The getter writes
    // only these three live outputs and neither retains pointers nor sets policy.
    let result =
        unsafe { task_policy_get(mach_task_self_, 1, &mut role, &mut count, &mut get_default) };
    if result != 0 || count != 1 || get_default != 0 {
        return Err(PlaybackThreadSchedulingError::new(
            "task_policy_get(TASK_CATEGORY_POLICY)",
            format!("native result={result}, count={count}, get_default={get_default}"),
        ));
    }
    Ok(role)
}

/// Observe CPU time and pthread QoS without changing scheduling policy.
///
/// CPU time excludes periods when the thread is asleep or not scheduled. The
/// Requested QoS and kernel priorities are separate observations. Their queries
/// are sequential, not an atomic scheduler snapshot or a bounded wake guarantee.
#[cfg(target_os = "macos")]
pub fn observe_macos_playback_thread(
) -> Result<MacPlaybackThreadObservation, PlaybackThreadSchedulingError> {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: The clock writes one initialized timespec and retains no pointer.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } != 0 {
        return Err(PlaybackThreadSchedulingError::new(
            "clock_gettime(CLOCK_THREAD_CPUTIME_ID)",
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let cpu_time_ns = u64::try_from(time.tv_sec)
        .ok()
        .filter(|_| (0..1_000_000_000).contains(&time.tv_nsec))
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .and_then(|seconds| seconds.checked_add(time.tv_nsec as u64))
        .ok_or_else(|| {
            PlaybackThreadSchedulingError::new("thread CPU clock", "invalid or overflowing time")
        })?;
    let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
    let mut priority = 0;
    // SAFETY: This queries only the live calling pthread and both output values.
    let result =
        unsafe { libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut priority) };
    if result != 0 {
        return Err(PlaybackThreadSchedulingError::new(
            "pthread_get_qos_class_np",
            std::io::Error::from_raw_os_error(result).to_string(),
        ));
    }
    let mut thread_id = 0;
    // SAFETY: This queries only the live calling pthread and its output value.
    let result = unsafe { libc::pthread_threadid_np(libc::pthread_self(), &mut thread_id) };
    if result != 0 {
        return Err(PlaybackThreadSchedulingError::new(
            "pthread_threadid_np",
            std::io::Error::from_raw_os_error(result).to_string(),
        ));
    }
    // SAFETY: All fields of this public SDK structure are integer values.
    let mut native: libc::thread_extended_info = unsafe { std::mem::zeroed() };
    let mut count = libc::THREAD_EXTENDED_INFO_COUNT;
    // SAFETY: pthread_mach_thread_np borrows the live calling pthread's port;
    // no send right is acquired. thread_info writes only this full-sized buffer
    // and count, retains neither pointer, and does not change thread policy.
    let result = unsafe {
        libc::thread_info(
            libc::pthread_mach_thread_np(libc::pthread_self()),
            libc::THREAD_EXTENDED_INFO as libc::thread_flavor_t,
            (&mut native as *mut libc::thread_extended_info).cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS || count != libc::THREAD_EXTENDED_INFO_COUNT {
        return Err(PlaybackThreadSchedulingError::new(
            "thread_info(THREAD_EXTENDED_INFO)",
            format!("native result={result}, count={count}"),
        ));
    }
    Ok(MacPlaybackThreadObservation {
        thread_id,
        cpu_time_ns,
        requested_qos_class: class as u32,
        relative_priority: priority,
        base_priority: native.pth_priority,
        current_priority: native.pth_curpri,
        maximum_priority: native.pth_maxpriority,
        scheduling_policy: native.pth_policy,
    })
}

/// Thread-affine native scheduling state for the product playback coordinator.
///
/// The owner synchronizes this object with transport residency. On Windows it
/// joins the MMCSS `Playback` task at critical relative priority and reverts that
/// registration when playback stops or the owner is dropped. It deliberately
/// does not change process-wide timer resolution or worker-pool policy.
#[derive(Debug, Default)]
pub struct PlaybackThreadScheduling {
    native: Option<NativePlaybackThreadScheduling>,
    portable_fallback_active: bool,
}

impl PlaybackThreadScheduling {
    /// Enter or leave the native scheduling class to match playback residency.
    ///
    /// A failed activation is reported once for the current active residency,
    /// then remains observable as `PortableFallback`; calling with
    /// `active = false` resets the residency and permits a later native retry.
    pub fn synchronize(
        &mut self,
        active: bool,
    ) -> Result<PlaybackThreadSchedulingStatus, PlaybackThreadSchedulingError> {
        if !active {
            self.native = None;
            self.portable_fallback_active = false;
            return Ok(PlaybackThreadSchedulingStatus::Inactive);
        }
        if self.native.is_some() {
            return Ok(PlaybackThreadSchedulingStatus::Active);
        }
        if self.portable_fallback_active {
            return Ok(PlaybackThreadSchedulingStatus::PortableFallback);
        }
        match NativePlaybackThreadScheduling::enter() {
            Ok(Some(native)) => {
                self.native = Some(native);
                Ok(PlaybackThreadSchedulingStatus::Active)
            }
            Ok(None) => {
                self.portable_fallback_active = true;
                Ok(PlaybackThreadSchedulingStatus::PortableFallback)
            }
            Err(error) => {
                self.portable_fallback_active = true;
                Err(error)
            }
        }
    }
}

#[cfg(target_os = "windows")]
struct NativePlaybackThreadScheduling {
    handle: windows_sys::Win32::Foundation::HANDLE,
    _thread_affine: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(target_os = "windows")]
impl std::fmt::Debug for NativePlaybackThreadScheduling {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("NativePlaybackThreadScheduling").finish_non_exhaustive()
    }
}

#[cfg(target_os = "windows")]
impl NativePlaybackThreadScheduling {
    fn enter() -> Result<Option<Self>, PlaybackThreadSchedulingError> {
        use windows_sys::Win32::System::Threading::{
            AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
            AVRT_PRIORITY_CRITICAL,
        };

        const PLAYBACK_TASK: &[u16] = &[
            b'P' as u16,
            b'l' as u16,
            b'a' as u16,
            b'y' as u16,
            b'b' as u16,
            b'a' as u16,
            b'c' as u16,
            b'k' as u16,
            0,
        ];
        let mut task_index = 0_u32;
        // SAFETY: PLAYBACK_TASK is a static NUL-terminated UTF-16 string and
        // task_index remains valid for the duration of the call.
        let handle =
            unsafe { AvSetMmThreadCharacteristicsW(PLAYBACK_TASK.as_ptr(), &mut task_index) };
        if handle.is_null() {
            return Err(PlaybackThreadSchedulingError::new(
                "AvSetMmThreadCharacteristicsW",
                std::io::Error::last_os_error().to_string(),
            ));
        }
        // SAFETY: handle is the live registration returned above and is owned
        // by this current thread until reverted below or in Drop.
        if unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL) } == 0 {
            let error = std::io::Error::last_os_error().to_string();
            // SAFETY: the handle is still live and owned by this thread.
            unsafe { AvRevertMmThreadCharacteristics(handle) };
            return Err(PlaybackThreadSchedulingError::new(
                "AvSetMmThreadPriority",
                error,
            ));
        }
        Ok(Some(Self {
            handle,
            _thread_affine: std::marker::PhantomData,
        }))
    }
}

#[cfg(target_os = "windows")]
impl Drop for NativePlaybackThreadScheduling {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::AvRevertMmThreadCharacteristics;

        // SAFETY: this non-Send guard can only be dropped on the thread that
        // owns its still-live MMCSS registration.
        unsafe { AvRevertMmThreadCharacteristics(self.handle) };
    }
}

#[cfg(target_os = "macos")]
struct NativePlaybackThreadScheduling {
    previous_class: libc::qos_class_t,
    previous_priority: i32,
    _thread_affine: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(target_os = "macos")]
impl std::fmt::Debug for NativePlaybackThreadScheduling {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("NativePlaybackThreadScheduling").finish_non_exhaustive()
    }
}

#[cfg(target_os = "macos")]
impl NativePlaybackThreadScheduling {
    fn enter() -> Result<Option<Self>, PlaybackThreadSchedulingError> {
        let mut previous_class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut previous_priority = 0_i32;
        // SAFETY: This reads only the current live pthread and initializes both
        // output values without retaining either pointer.
        let query_result = unsafe {
            libc::pthread_get_qos_class_np(
                libc::pthread_self(),
                &mut previous_class,
                &mut previous_priority,
            )
        };
        if query_result != 0 {
            return Err(PlaybackThreadSchedulingError::new(
                "pthread_get_qos_class_np",
                std::io::Error::from_raw_os_error(query_result).to_string(),
            ));
        }
        // SAFETY: This changes only the current thread. The thread-affine guard
        // restores the captured class before it can leave that thread.
        let result = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if result != 0 {
            return Err(PlaybackThreadSchedulingError::new(
                "pthread_set_qos_class_self_np",
                std::io::Error::from_raw_os_error(result).to_string(),
            ));
        }
        let native = Self {
            previous_class,
            previous_priority,
            _thread_affine: std::marker::PhantomData,
        };
        static DIAGNOSTICS_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DIAGNOSTICS_ENABLED.get_or_init(|| {
            std::env::var_os("MONDRIAN_PREVIEW_WAIT_DIAGNOSTICS").is_some_and(|value| value == "1")
        }) {
            // The same optional observation covers Window and Headless owners.
            // Keep the guard alive so unwinding also restores requested QoS.
            eprintln!(
                "MONDRIAN_MACOS_PLAYBACK_THREAD task_role={:?} thread={:?}",
                observe_macos_playback_task_role(),
                observe_macos_playback_thread(),
            );
        }
        Ok(Some(native))
    }
}

#[cfg(target_os = "macos")]
impl Drop for NativePlaybackThreadScheduling {
    fn drop(&mut self) {
        // SAFETY: The non-Send guard is dropped on the owning pthread and
        // restores the exact QoS class captured before admission.
        let result = unsafe {
            libc::pthread_set_qos_class_self_np(self.previous_class, self.previous_priority)
        };
        debug_assert_eq!(result, 0, "failed to restore macOS playback thread QoS");
    }
}

#[cfg(target_os = "linux")]
struct NativePlaybackThreadScheduling {
    thread_id: libc::pid_t,
    previous_nice: i32,
    _thread_affine: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for NativePlaybackThreadScheduling {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("NativePlaybackThreadScheduling").finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl NativePlaybackThreadScheduling {
    fn enter() -> Result<Option<Self>, PlaybackThreadSchedulingError> {
        // Linux applies PRIO_PROCESS nice values per thread when addressed by
        // TID. This improves UI/render admission without placing a fallible
        // desktop event loop in a realtime scheduling class.
        let thread_id = unsafe { libc::gettid() };
        // SAFETY: errno is thread-local. Clearing it disambiguates a valid -1
        // nice value from getpriority failure.
        unsafe { *libc::__errno_location() = 0 };
        let previous_nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, thread_id as u32) };
        let query_error = unsafe { *libc::__errno_location() };
        if query_error != 0 {
            return Err(PlaybackThreadSchedulingError::new(
                "getpriority(PRIO_PROCESS)",
                std::io::Error::from_raw_os_error(query_error).to_string(),
            ));
        }
        let requested_nice = previous_nice.saturating_sub(5).max(-10);
        let result =
            unsafe { libc::setpriority(libc::PRIO_PROCESS, thread_id as u32, requested_nice) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES)) {
                return Ok(None);
            }
            return Err(PlaybackThreadSchedulingError::new(
                "setpriority(PRIO_PROCESS)",
                error.to_string(),
            ));
        }
        if requested_nice == previous_nice {
            return Ok(None);
        }
        Ok(Some(Self {
            thread_id,
            previous_nice,
            _thread_affine: std::marker::PhantomData,
        }))
    }
}

#[cfg(target_os = "linux")]
impl Drop for NativePlaybackThreadScheduling {
    fn drop(&mut self) {
        // SAFETY: The non-Send guard is dropped on the owning thread and
        // restores the exact per-thread nice value captured before admission.
        let result = unsafe {
            libc::setpriority(
                libc::PRIO_PROCESS,
                self.thread_id as u32,
                self.previous_nice,
            )
        };
        debug_assert_eq!(
            result, 0,
            "failed to restore Linux playback thread nice value"
        );
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
#[derive(Debug)]
struct NativePlaybackThreadScheduling;

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
impl NativePlaybackThreadScheduling {
    fn enter() -> Result<Option<Self>, PlaybackThreadSchedulingError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn native_playback_residency_restores_calling_thread_qos_on_stop_and_drop() {
        let before = observe_macos_playback_thread().expect("read actual calling pthread");
        let task_role = observe_macos_playback_task_role().expect("read actual calling task role");
        let mut scheduling = PlaybackThreadScheduling::default();
        for _ in 0..2 {
            assert_eq!(
                scheduling.synchronize(true).expect("native playback admission"),
                PlaybackThreadSchedulingStatus::Active
            );
            let active = observe_macos_playback_thread().expect("read admitted pthread");
            assert_eq!(active.thread_id, before.thread_id);
            assert_eq!(
                active.requested_qos_class,
                libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE as u32
            );
            assert_eq!(active.relative_priority, 0);
            assert_eq!(
                scheduling.synchronize(false).expect("consuming residency release"),
                PlaybackThreadSchedulingStatus::Inactive
            );
            let stopped = observe_macos_playback_thread().expect("read restored pthread");
            assert_eq!(
                (stopped.requested_qos_class, stopped.relative_priority),
                (before.requested_qos_class, before.relative_priority)
            );
        }
        scheduling.synchronize(true).expect("reopen native playback residency");
        drop(scheduling);
        let dropped = observe_macos_playback_thread().expect("read pthread after Drop");
        assert_eq!(
            observe_macos_playback_task_role().expect("read task role after Drop"),
            task_role
        );
        assert_eq!(
            (dropped.requested_qos_class, dropped.relative_priority),
            (before.requested_qos_class, before.relative_priority)
        );
    }

    #[test]
    fn portable_fallback_is_stable_until_residency_ends() {
        let mut scheduling =
            PlaybackThreadScheduling { native: None, portable_fallback_active: true };

        assert_eq!(
            scheduling.synchronize(true).expect("fallback status"),
            PlaybackThreadSchedulingStatus::PortableFallback
        );
        assert_eq!(
            scheduling.synchronize(false).expect("inactive status"),
            PlaybackThreadSchedulingStatus::Inactive
        );
        assert!(!scheduling.portable_fallback_active);
    }
}
