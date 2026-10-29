use super::*;
use std::cell::RefCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Register(RealtimeAudioTask),
    Priority(usize, i32),
    Revert(usize),
}

struct State {
    current: u32,
    calls: Vec<Call>,
    register_error: Option<u32>,
    priority_error: Option<u32>,
    revert_errors: std::collections::VecDeque<u32>,
}

#[derive(Clone)]
struct Mock(Rc<RefCell<State>>);
impl Mock {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(State {
            current: 37,
            calls: Vec::new(),
            register_error: None,
            priority_error: None,
            revert_errors: Default::default(),
        })))
    }
}
impl Backend for Mock {
    fn current_thread(&self) -> Result<u32, AudioThreadSchedulingError> {
        Ok(self.0.borrow().current)
    }
    fn register(&self, task: RealtimeAudioTask) -> Result<(usize, u32), u32> {
        let mut state = self.0.borrow_mut();
        state.calls.push(Call::Register(task));
        state.register_error.map_or(Ok((73, 5)), Err)
    }
    fn priority(&self, handle: usize, value: i32) -> Result<(), u32> {
        let mut state = self.0.borrow_mut();
        state.calls.push(Call::Priority(handle, value));
        state.priority_error.map_or(Ok(()), Err)
    }
    fn revert(&self, handle: usize) -> Result<(), u32> {
        let mut state = self.0.borrow_mut();
        state.calls.push(Call::Revert(handle));
        state.revert_errors.pop_front().map_or(Ok(()), Err)
    }
}

// Every scenario owns a fresh real test thread: TLS poison cannot spill between
// independent tests, and mocks never manufacture a cross-thread lease transfer.
fn fresh(work: impl FnOnce() + Send + 'static) {
    std::thread::spawn(work).join().expect("test thread");
}

#[test]
fn scheduling_producer_native_calls_and_confirmed_exit_preserve_exact_entry() {
    fresh(|| {
        let backend = Mock::new();
        let lease = Lease::enter(backend.clone(), RealtimeAudioThreadRole::PlaybackProducer)
            .expect("admit");
        assert_eq!(lease.evidence.native_thread_id(), 37);
        assert_eq!(lease.evidence.task_index(), 5);
        assert_eq!(
            backend.0.borrow().calls,
            [
                Call::Register(RealtimeAudioTask::Audio),
                Call::Priority(73, 0)
            ]
        );
        let entry = lease.evidence;
        assert_eq!(
            lease.finish().expect("confirmed restore").registration(),
            entry
        );
        assert_eq!(backend.0.borrow().calls.last(), Some(&Call::Revert(73)));
        assert!(!THREAD_ROLE.with(|state| state.get().active));
    });
}

#[test]
fn scheduling_dsp_selects_pro_audio_high_and_rejects_nested_before_native_call() {
    fresh(|| {
        let backend = Mock::new();
        let lease =
            Lease::enter(backend.clone(), RealtimeAudioThreadRole::NativeDsp).expect("admit");
        assert_eq!(
            backend.0.borrow().calls,
            [
                Call::Register(RealtimeAudioTask::ProAudio),
                Call::Priority(73, 1)
            ]
        );
        assert!(matches!(
            Lease::enter(backend.clone(), RealtimeAudioThreadRole::PlaybackProducer),
            Err(AudioThreadSchedulingError::RoleAlreadyActive)
        ));
        assert_eq!(backend.0.borrow().calls.len(), 2);
        lease.finish().expect("restore");
    });
}

#[test]
fn scheduling_registration_refusal_does_not_revert_unowned_handle_or_poison_thread() {
    fresh(|| {
        let backend = Mock::new();
        backend.0.borrow_mut().register_error = Some(1062);
        assert!(matches!(
            Lease::enter(backend.clone(), RealtimeAudioThreadRole::NativeDsp),
            Err(AudioThreadSchedulingError::RegistrationRefused { code: 1062 })
        ));
        assert_eq!(
            backend.0.borrow().calls,
            [Call::Register(RealtimeAudioTask::ProAudio)]
        );
        backend.0.borrow_mut().register_error = None;
        Lease::enter(backend, RealtimeAudioThreadRole::NativeDsp)
            .expect("later admission")
            .finish()
            .expect("restore");
    });
}

#[test]
fn scheduling_priority_refusal_cleans_up_before_another_admission() {
    fresh(|| {
        let backend = Mock::new();
        backend.0.borrow_mut().priority_error = Some(5);
        assert!(matches!(
            Lease::enter(backend.clone(), RealtimeAudioThreadRole::NativeDsp),
            Err(AudioThreadSchedulingError::PriorityRefused { code: 5 })
        ));
        assert_eq!(backend.0.borrow().calls.last(), Some(&Call::Revert(73)));
        backend.0.borrow_mut().priority_error = None;
        Lease::enter(backend, RealtimeAudioThreadRole::NativeDsp)
            .expect("later admission")
            .finish()
            .expect("restore");
    });
}

#[test]
fn scheduling_failed_partial_admission_cleanup_requires_physical_thread_exit() {
    fresh(|| {
        let backend = Mock::new();
        backend.0.borrow_mut().priority_error = Some(5);
        backend.0.borrow_mut().revert_errors.push_back(6);
        assert!(matches!(
            Lease::enter(backend.clone(), RealtimeAudioThreadRole::NativeDsp),
            Err(AudioThreadSchedulingError::PriorityAndRestoreRefused {
                priority_code: 5,
                restore_code: 6
            })
        ));
        let calls = backend.0.borrow().calls.len();
        assert!(matches!(
            Lease::enter(backend.clone(), RealtimeAudioThreadRole::NativeDsp),
            Err(AudioThreadSchedulingError::PreviousRestoreUnconfirmed)
        ));
        assert_eq!(backend.0.borrow().calls.len(), calls);
    });
}

#[test]
fn scheduling_explicit_restore_refusal_is_not_converted_to_success_by_drop_retry() {
    fresh(|| {
        let backend = Mock::new();
        let lease =
            Lease::enter(backend.clone(), RealtimeAudioThreadRole::NativeDsp).expect("admit");
        backend.0.borrow_mut().revert_errors.push_back(6);
        assert_eq!(
            lease.finish(),
            Err(AudioThreadSchedulingError::RestoreRefused { code: 6 })
        );
        assert_eq!(
            backend
                .0
                .borrow()
                .calls
                .iter()
                .filter(|call| matches!(call, Call::Revert(73)))
                .count(),
            2
        );
        assert!(matches!(
            Lease::enter(backend, RealtimeAudioThreadRole::NativeDsp),
            Err(AudioThreadSchedulingError::PreviousRestoreUnconfirmed)
        ));
    });
}

#[test]
fn scheduling_unwind_fallback_reverts_same_thread_registration_once() {
    fresh(|| {
        let backend = Mock::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease =
                Lease::enter(backend.clone(), RealtimeAudioThreadRole::NativeDsp).expect("admit");
            panic!("owned test panic");
        }));
        assert!(result.is_err());
        assert_eq!(
            backend
                .0
                .borrow()
                .calls
                .iter()
                .filter(|call| matches!(call, Call::Revert(73)))
                .count(),
            1
        );
        assert!(!THREAD_ROLE.with(|state| state.get().active));
    });
}

#[test]
fn scheduling_wrong_physical_identity_never_calls_native_revert() {
    fresh(|| {
        let backend = Mock::new();
        let lease =
            Lease::enter(backend.clone(), RealtimeAudioThreadRole::NativeDsp).expect("admit");
        // Backend fault injection, not moving the !Send guard to another thread.
        backend.0.borrow_mut().current = 38;
        assert_eq!(
            lease.finish(),
            Err(AudioThreadSchedulingError::WrongThread { owner: 37, current: 38 })
        );
        assert_eq!(backend.0.borrow().calls.len(), 2);
    });
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "real Windows MMCSS admission/restore; run explicitly on target machine"]
fn scheduling_actual_windows_dedicated_thread_entry_exit_and_reentry() {
    fresh(|| {
        let lease = RealtimeAudioThreadLease::enter(RealtimeAudioThreadRole::NativeDsp)
            .expect("actual MMCSS admission");
        let entry = lease.evidence();
        // SAFETY: documented calling-thread identity query has no owned input.
        assert_eq!(entry.native_thread_id(), unsafe {
            windows_sys::Win32::System::Threading::GetCurrentThreadId()
        });
        assert_eq!(entry.task(), RealtimeAudioTask::ProAudio);
        assert_eq!(entry.relative_priority(), 1);
        assert_eq!(lease.finish().expect("actual revert").registration(), entry);
        RealtimeAudioThreadLease::enter(RealtimeAudioThreadRole::PlaybackProducer)
            .expect("actual reentry")
            .finish()
            .expect("actual second revert");
    });
}

#[cfg(not(target_os = "windows"))]
#[test]
fn scheduling_unsupported_platform_has_no_ordinary_realtime_success() {
    assert!(matches!(
        RealtimeAudioThreadLease::enter(RealtimeAudioThreadRole::NativeDsp),
        Err(AudioThreadSchedulingError::UnsupportedPlatform)
    ));
}
