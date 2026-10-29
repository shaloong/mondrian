//! Actual thread identity for the one current CLAP audio role.
//!
//! Main identity remains the initializing Rust ThreadId. The one borrowed audio
//! role may run on the joined scoped DSP thread on every native platform, so its
//! current identity is a separate scalar fact. No process-wide role, TLS registry,
//! lock, allocation or broad Send implementation is introduced.

use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::Ordering;

#[cfg(windows)]
use std::sync::atomic::AtomicU32 as AtomicThreadIdentity;
#[cfg(unix)]
use std::sync::atomic::AtomicUsize as AtomicThreadIdentity;

#[cfg(windows)]
type ThreadIdentity = u32;
#[cfg(unix)]
type ThreadIdentity = usize;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    // Win32 DWORD is the exact documented u32 ABI, not Rust ThreadId's private
    // representation. GetCurrentThreadId has no ownership or failure result.
    fn GetCurrentThreadId() -> u32;
}

fn current_thread_identity() -> ThreadIdentity {
    #[cfg(windows)]
    {
        // SAFETY: this takes no pointers and reads the calling Win32 thread.
        unsafe { GetCurrentThreadId() }
    }
    #[cfg(unix)]
    {
        // SAFETY: pthread_self has no arguments or owned result. Linux/macOS
        // expose pthread_t as integral/pointer identity bits; copying them does
        // not dereference a handle. The guard keeps the marked thread's scope
        // alive, and background observers only compare their own identity.
        unsafe { libc::pthread_self() as usize }
    }
}

pub(super) struct ClapAudioRole {
    owner: AtomicThreadIdentity,
}

impl ClapAudioRole {
    pub(super) const fn new() -> Self {
        Self { owner: AtomicThreadIdentity::new(0) }
    }

    /// Only a caller holding the exclusive Clack audio processor may enter.
    /// The borrowed split/join guarantees there is no concurrent lifecycle or
    /// second audio processor. Nested markers restore the enclosing owner;
    /// they do not grant or detect plugin processing reentry.
    pub(super) fn enter(&self) -> ClapAudioThreadGuard<'_> {
        let current = current_thread_identity();
        let previous = self.owner.load(Ordering::Acquire);
        if current == 0
            || (previous != 0 && previous != current)
            || self
                .owner
                .compare_exchange(previous, current, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            // The exclusive native borrow was violated by the host. Never
            // overwrite a live role or continue with false thread identity.
            // Retire the supervised child without unwinding through native code.
            std::process::abort();
        }
        ClapAudioThreadGuard {
            role: self,
            previous,
            owner: current,
            owner_thread: PhantomData,
        }
    }

    pub(super) fn is_current(&self) -> bool {
        let owner = self.owner.load(Ordering::Acquire);
        owner != 0 && owner == current_thread_identity()
    }
}

/// The !Send/!Sync marker requires restoration on the actual owner thread.
pub(super) struct ClapAudioThreadGuard<'a> {
    role: &'a ClapAudioRole,
    previous: ThreadIdentity,
    owner: ThreadIdentity,
    owner_thread: PhantomData<Rc<()>>,
}

impl Drop for ClapAudioThreadGuard<'_> {
    fn drop(&mut self) {
        debug_assert_eq!(current_thread_identity(), self.owner);
        self.role.owner.store(self.previous, Ordering::Release);
    }
}

#[cfg(test)]
mod distinct_role_contract_tests {
    use super::*;
    #[test]
    fn actual_scoped_audio_thread_does_not_mark_main_or_background_as_audio() {
        let role = ClapAudioRole::new();
        let entered = std::sync::Barrier::new(2);
        let retire = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _audio = role.enter();
                assert!(role.is_current());
                entered.wait();
                retire.wait();
            });
            entered.wait();
            assert!(!role.is_current());
            scope.spawn(|| assert!(!role.is_current())).join().expect("background observer");
            retire.wait();
        });
        assert!(!role.is_current());
    }

    #[test]
    fn symbolic_main_audio_and_nested_markers_restore_the_previous_owner() {
        let role = ClapAudioRole::new();
        assert!(!role.is_current());
        let outer = role.enter();
        assert!(role.is_current());
        {
            let _nested = role.enter();
            assert!(role.is_current());
        }
        assert!(role.is_current());
        drop(outer);
        assert!(!role.is_current());
    }
}
