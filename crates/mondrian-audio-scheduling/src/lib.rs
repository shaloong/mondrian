//! Narrow native scheduling ownership for actual audio producer and DSP threads.
//!
//! The existing render mode selects whether a caller requests this native role.
//! Offline work does not register. UI/main/reader threads do not acquire it.
//! A successful MMCSS registration is an OS scheduling fact, never a hard
//! realtime, callback-deadline, device-delivery or cross-platform guarantee.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Dedicated physical execution responsibility requesting native scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeAudioThreadRole {
    /// Sole queued PCM producer, separate from the physical device callback.
    PlaybackProducer,
    /// Borrowed exclusive native DSP role, separate from main/editor/reader.
    NativeDsp,
}

/// Native Windows task selected for this one physical responsibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeAudioTask {
    /// MMCSS Audio task for the queued PCM producer.
    Audio,
    /// MMCSS Pro Audio task for the dedicated native DSP executor.
    ProAudio,
}

/// Successful native registration facts on the actual executing OS thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeAudioThreadEvidence {
    role: RealtimeAudioThreadRole,
    task: RealtimeAudioTask,
    native_thread_id: u32,
    task_index: u32,
    relative_priority: i32,
}

impl RealtimeAudioThreadEvidence {
    /// Physical responsibility that requested this scheduling registration.
    pub const fn role(self) -> RealtimeAudioThreadRole {
        self.role
    }
    /// Actual Windows task accepted by the native registration call.
    pub const fn task(self) -> RealtimeAudioTask {
        self.task
    }
    /// OS thread identity returned by GetCurrentThreadId on entry.
    pub const fn native_thread_id(self) -> u32 {
        self.native_thread_id
    }
    /// Native task index returned by AvSetMmThreadCharacteristicsW.
    pub const fn task_index(self) -> u32 {
        self.task_index
    }
    /// Requested priority within the accepted task, not a kernel-priority claim.
    pub const fn relative_priority(self) -> i32 {
        self.relative_priority
    }
}

/// Confirmed native restoration after this exact same-thread registration.
///
/// Only a successful explicit native revert can construct this value. The entry
/// facts remain scheduling facts; restoration does not prove a callback deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeAudioThreadExitEvidence {
    registration: RealtimeAudioThreadEvidence,
}

impl RealtimeAudioThreadExitEvidence {
    /// Exact entry registration whose same-thread native revert succeeded.
    pub const fn registration(self) -> RealtimeAudioThreadEvidence {
        self.registration
    }
}

/// Exact native-role admission, ownership or restoration refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AudioThreadSchedulingError {
    /// This adapter has no qualified native implementation for this platform.
    #[error("native audio scheduling is unavailable on this platform")]
    UnsupportedPlatform,
    /// One physical thread already holds a role from this adapter.
    #[error("native audio scheduling roles cannot nest on one thread")]
    RoleAlreadyActive,
    /// A previous unconfirmed restore prohibits another role on this thread.
    #[error("native audio scheduling state is unconfirmed after a restore failure")]
    PreviousRestoreUnconfirmed,
    /// Native task admission failed before DSP/PCM work was allowed.
    #[error("native audio task registration failed with Windows error {code}")]
    RegistrationRefused {
        /// Exact GetLastError result, not a localized guessed message.
        code: u32,
    },
    /// Priority admission failed; the newly registered task was reverted.
    #[error("native audio task priority failed with Windows error {code}")]
    PriorityRefused {
        /// Exact failing priority-call GetLastError result.
        code: u32,
    },
    /// Priority failed and cleanup could not confirm ordinary scheduling.
    #[error("native audio task priority failed with Windows error {priority_code}; restore failed with {restore_code}")]
    PriorityAndRestoreRefused {
        /// Original failing priority-call GetLastError result.
        priority_code: u32,
        /// Exact failing cleanup-call GetLastError result.
        restore_code: u32,
    },
    /// Exit could not confirm that this thread returned to ordinary scheduling.
    #[error("native audio task restoration failed with Windows error {code}")]
    RestoreRefused {
        /// Exact failing revert-call GetLastError result.
        code: u32,
    },
    /// Ownership does not match the thread performing native restoration.
    #[error("native audio task belongs to thread {owner}, current thread is {current}")]
    WrongThread {
        /// Physical OS thread that actually registered the task.
        owner: u32,
        /// Physical OS thread attempting restoration.
        current: u32,
    },
}

/// Process-local fallback-exit facts; unrelated to author or playback authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSchedulingExitDiagnostics {
    /// Guards dropped without an explicit finish call.
    pub implicit_exits: u64,
    /// Actual native restore attempts that returned failure.
    pub restore_failures: u64,
}

static IMPLICIT_EXITS: AtomicU64 = AtomicU64::new(0);
static RESTORE_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Read bounded native-role fallback diagnostics without allocating or locking.
pub fn audio_scheduling_exit_diagnostics() -> AudioSchedulingExitDiagnostics {
    AudioSchedulingExitDiagnostics {
        implicit_exits: IMPLICIT_EXITS.load(Ordering::Acquire),
        restore_failures: RESTORE_FAILURES.load(Ordering::Acquire),
    }
}

#[derive(Clone, Copy, Default)]
struct ThreadRoleState {
    active: bool,
    restore_unconfirmed: bool,
}
thread_local! {
    static THREAD_ROLE: Cell<ThreadRoleState> = const { Cell::new(ThreadRoleState {
        active: false, restore_unconfirmed: false,
    }) };
}

trait Backend {
    fn current_thread(&self) -> Result<u32, AudioThreadSchedulingError>;
    fn register(&self, task: RealtimeAudioTask) -> Result<(usize, u32), u32>;
    fn priority(&self, registration: usize, priority: i32) -> Result<(), u32>;
    fn revert(&self, registration: usize) -> Result<(), u32>;
}

struct NativeBackend;

struct Lease<B: Backend> {
    backend: B,
    registration: Option<usize>,
    evidence: RealtimeAudioThreadEvidence,
    explicit_exit: bool,
    _same_thread: PhantomData<Rc<()>>,
}

impl<B: Backend> Lease<B> {
    fn enter(
        backend: B,
        role: RealtimeAudioThreadRole,
    ) -> Result<Self, AudioThreadSchedulingError> {
        let owner = backend.current_thread()?;
        THREAD_ROLE.with(|state| {
            let current = state.get();
            if current.restore_unconfirmed {
                return Err(AudioThreadSchedulingError::PreviousRestoreUnconfirmed);
            }
            if current.active {
                return Err(AudioThreadSchedulingError::RoleAlreadyActive);
            }
            Ok(())
        })?;
        let (task, priority) = match role {
            RealtimeAudioThreadRole::PlaybackProducer => (RealtimeAudioTask::Audio, 0),
            RealtimeAudioThreadRole::NativeDsp => (RealtimeAudioTask::ProAudio, 1),
        };
        let (registration, task_index) = backend
            .register(task)
            .map_err(|code| AudioThreadSchedulingError::RegistrationRefused { code })?;
        if let Err(priority_code) = backend.priority(registration, priority) {
            return match backend.revert(registration) {
                Ok(()) => Err(AudioThreadSchedulingError::PriorityRefused { code: priority_code }),
                Err(restore_code) => {
                    THREAD_ROLE.with(|state| {
                        state.set(ThreadRoleState { active: false, restore_unconfirmed: true })
                    });
                    RESTORE_FAILURES.fetch_add(1, Ordering::AcqRel);
                    Err(AudioThreadSchedulingError::PriorityAndRestoreRefused {
                        priority_code,
                        restore_code,
                    })
                }
            };
        }
        THREAD_ROLE
            .with(|state| state.set(ThreadRoleState { active: true, restore_unconfirmed: false }));
        Ok(Self {
            backend,
            registration: Some(registration),
            evidence: RealtimeAudioThreadEvidence {
                role,
                task,
                native_thread_id: owner,
                task_index,
                relative_priority: priority,
            },
            explicit_exit: false,
            _same_thread: PhantomData,
        })
    }

    fn finish(mut self) -> Result<RealtimeAudioThreadExitEvidence, AudioThreadSchedulingError> {
        self.explicit_exit = true;
        self.restore()?;
        Ok(RealtimeAudioThreadExitEvidence { registration: self.evidence })
    }

    fn restore(&mut self) -> Result<(), AudioThreadSchedulingError> {
        let Some(registration) = self.registration else {
            return Ok(());
        };
        let current = self.backend.current_thread()?;
        if current != self.evidence.native_thread_id {
            return Err(AudioThreadSchedulingError::WrongThread {
                owner: self.evidence.native_thread_id,
                current,
            });
        }
        if let Err(code) = self.backend.revert(registration) {
            THREAD_ROLE.with(|state| {
                state.set(ThreadRoleState { active: true, restore_unconfirmed: true })
            });
            RESTORE_FAILURES.fetch_add(1, Ordering::AcqRel);
            return Err(AudioThreadSchedulingError::RestoreRefused { code });
        }
        self.registration = None;
        THREAD_ROLE.with(|state| {
            // An earlier failed attempt remains explicit unconfirmed admission,
            // even if Drop's single bounded cleanup attempt succeeds later.
            let restore_unconfirmed = state.get().restore_unconfirmed;
            state.set(ThreadRoleState { active: false, restore_unconfirmed });
        });
        Ok(())
    }
}

impl<B: Backend> Drop for Lease<B> {
    fn drop(&mut self) {
        if self.registration.is_none() {
            return;
        }
        if !self.explicit_exit {
            IMPLICIT_EXITS.fetch_add(1, Ordering::AcqRel);
        }
        let _ = self.restore();
    }
}

/// Same-thread native scheduling lease for one actual producer or DSP scope.
///
/// Acquire before readiness/first processing, explicitly finish after the role
/// stops, and consume its Result before declaring shutdown/restoration complete.
/// Drop performs one bounded fallback restore and exposes failures in diagnostics;
/// it cannot claim successful restoration. A failed explicit restore poisons
/// further admission on that physical thread. No per-callback allocation is added.
///
/// The lease cannot move to another thread or be shared across threads:
/// ```compile_fail
/// use mondrian_audio_scheduling::RealtimeAudioThreadLease;
/// fn require_send<T: Send>(_: T) {}
/// fn invalid(lease: RealtimeAudioThreadLease) { require_send(lease); }
/// ```
/// ```compile_fail
/// use mondrian_audio_scheduling::RealtimeAudioThreadLease;
/// fn require_sync<T: Sync>() {}
/// fn invalid() { require_sync::<RealtimeAudioThreadLease>(); }
/// ```
pub struct RealtimeAudioThreadLease {
    inner: Lease<NativeBackend>,
}

impl RealtimeAudioThreadLease {
    /// Request native scheduling on this dedicated physical realtime executor.
    /// Offline and UI/main/reader callers must not request this role.
    pub fn enter(role: RealtimeAudioThreadRole) -> Result<Self, AudioThreadSchedulingError> {
        Ok(Self { inner: Lease::enter(NativeBackend, role)? })
    }
    /// Successful native entry facts, not a performance or device-ready result.
    pub const fn evidence(&self) -> RealtimeAudioThreadEvidence {
        self.inner.evidence
    }
    /// Explicitly restore ordinary scheduling on the same physical thread.
    pub fn finish(self) -> Result<RealtimeAudioThreadExitEvidence, AudioThreadSchedulingError> {
        self.inner.finish()
    }
    /// Inline guard layout; do not double-charge it inside an accounted stack.
    pub const fn inline_layout() -> std::alloc::Layout {
        std::alloc::Layout::new::<Self>()
    }
}

#[cfg(test)]
mod tests;

#[cfg(target_os = "windows")]
impl Backend for NativeBackend {
    fn current_thread(&self) -> Result<u32, AudioThreadSchedulingError> {
        // SAFETY: this Windows primitive queries only the calling OS thread.
        Ok(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() })
    }
    fn register(&self, task: RealtimeAudioTask) -> Result<(usize, u32), u32> {
        let name = match task {
            RealtimeAudioTask::Audio => windows_sys::w!("Audio"),
            RealtimeAudioTask::ProAudio => windows_sys::w!("Pro Audio"),
        };
        let mut index = 0;
        // SAFETY: static UTF16 task is NUL-terminated, index is live unique output.
        let handle = unsafe {
            windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW(name, &mut index)
        };
        if handle.is_null() {
            // SAFETY: capture this call's error immediately on the same thread.
            Err(unsafe { windows_sys::Win32::Foundation::GetLastError() })
        } else {
            Ok((handle as usize, index))
        }
    }
    fn priority(&self, registration: usize, priority: i32) -> Result<(), u32> {
        // SAFETY: registration is a live handle from this same Backend/thread.
        let accepted = unsafe {
            windows_sys::Win32::System::Threading::AvSetMmThreadPriority(
                registration as windows_sys::Win32::Foundation::HANDLE,
                priority,
            )
        };
        if accepted == 0 {
            // SAFETY: capture the priority call's immediate thread-local error.
            Err(unsafe { windows_sys::Win32::Foundation::GetLastError() })
        } else {
            Ok(())
        }
    }
    fn revert(&self, registration: usize) -> Result<(), u32> {
        // SAFETY: Lease checks current physical owner before every restoration.
        let accepted = unsafe {
            windows_sys::Win32::System::Threading::AvRevertMmThreadCharacteristics(
                registration as windows_sys::Win32::Foundation::HANDLE,
            )
        };
        if accepted == 0 {
            // SAFETY: capture the revert call's immediate thread-local error.
            Err(unsafe { windows_sys::Win32::Foundation::GetLastError() })
        } else {
            Ok(())
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl Backend for NativeBackend {
    fn current_thread(&self) -> Result<u32, AudioThreadSchedulingError> {
        Err(AudioThreadSchedulingError::UnsupportedPlatform)
    }
    fn register(&self, _task: RealtimeAudioTask) -> Result<(usize, u32), u32> {
        Err(0)
    }
    fn priority(&self, _registration: usize, _priority: i32) -> Result<(), u32> {
        Err(0)
    }
    fn revert(&self, _registration: usize) -> Result<(), u32> {
        Err(0)
    }
}
