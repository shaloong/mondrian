//! Actual thread identity for the one current CLAP audio role.
//!
//! Main identity remains the initializing Rust ThreadId. On Windows the one
//! borrowed audio role may run on the joined scoped DSP thread, so its current
//! OS identity is a separate scalar fact. No process-wide role, TLS registry,
//! lock, allocation or broad Send implementation is introduced.

use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::Ordering;

#[cfg(not(windows))]
use std::sync::atomic::AtomicBool;
#[cfg(windows)]
use std::sync::atomic::AtomicU32;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    // Win32 DWORD is the exact documented u32 ABI, not Rust ThreadId's private
    // representation. GetCurrentThreadId has no ownership or failure result.
    fn GetCurrentThreadId() -> u32;
}

pub(super) struct ClapAudioRole {
    #[cfg(windows)]
    owner: AtomicU32,
    #[cfg(not(windows))]
    active: AtomicBool,
}

impl ClapAudioRole {
    pub(super) const fn new() -> Self {
        Self {
            #[cfg(windows)]
            owner: AtomicU32::new(0),
            #[cfg(not(windows))]
            active: AtomicBool::new(false),
        }
    }

    /// Only a caller holding the exclusive Clack audio processor may enter.
    /// The borrowed split/join guarantees there is no concurrent lifecycle or
    /// second audio processor. Nested markers restore the enclosing owner;
    /// they do not grant or detect plugin processing reentry.
    pub(super) fn enter(&self, main_thread: std::thread::ThreadId) -> ClapAudioThreadGuard<'_> {
        #[cfg(windows)]
        {
            let _ = main_thread;
            // SAFETY: this takes no pointers and reads the calling OS thread.
            let current = unsafe { GetCurrentThreadId() };
            let previous = self.owner.load(Ordering::Acquire);
            if (previous != 0 && previous != current)
                || self
                    .owner
                    .compare_exchange(previous, current, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                // The exclusive native borrow was violated by the host. Do not
                // overwrite another live thread's role or continue into native
                // processing/lifecycle with false thread identity. A supervised
                // child can be retired without unwinding through native unload.
                std::process::abort();
            }
            ClapAudioThreadGuard {
                role: self,
                previous,
                owner: current,
                owner_thread: PhantomData,
            }
        }
        #[cfg(not(windows))]
        {
            // Non-Windows continues the existing inline path. A distinct native
            // DSP role needs its own platform identity and scheduling adapter.
            debug_assert_eq!(std::thread::current().id(), main_thread);
            let previous = self.active.swap(true, Ordering::AcqRel);
            ClapAudioThreadGuard { role: self, previous, owner_thread: PhantomData }
        }
    }

    pub(super) fn is_current(&self, is_main: bool) -> bool {
        #[cfg(windows)]
        {
            let _ = is_main;
            // SAFETY: no pointers or retained OS handles are involved.
            let current = unsafe { GetCurrentThreadId() };
            self.owner.load(Ordering::Acquire) == current
        }
        #[cfg(not(windows))]
        {
            is_main && self.active.load(Ordering::Acquire)
        }
    }
}

/// The !Send/!Sync marker requires restoration on the actual owner thread.
pub(super) struct ClapAudioThreadGuard<'a> {
    role: &'a ClapAudioRole,
    #[cfg(windows)]
    previous: u32,
    #[cfg(windows)]
    owner: u32,
    #[cfg(not(windows))]
    previous: bool,
    owner_thread: PhantomData<Rc<()>>,
}

impl Drop for ClapAudioThreadGuard<'_> {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: no arguments, exact scalar Win32 identity only.
            debug_assert_eq!(unsafe { GetCurrentThreadId() }, self.owner);
            self.role.owner.store(self.previous, Ordering::Release);
        }
        #[cfg(not(windows))]
        self.role.active.store(self.previous, Ordering::Release);
    }
}

#[cfg(all(test, windows))]
mod distinct_role_contract_tests {
    use super::*;
    #[test]
    fn actual_scoped_audio_thread_does_not_mark_main_or_background_as_audio() {
        let role = ClapAudioRole::new();
        let main = std::thread::current().id();
        let entered = std::sync::Barrier::new(2);
        let retire = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _audio = role.enter(main);
                assert!(role.is_current(false));
                entered.wait();
                retire.wait();
            });
            entered.wait();
            assert!(!role.is_current(true));
            scope
                .spawn(|| assert!(!role.is_current(false)))
                .join()
                .expect("background observer");
            retire.wait();
        });
        assert!(!role.is_current(true));
    }
}
