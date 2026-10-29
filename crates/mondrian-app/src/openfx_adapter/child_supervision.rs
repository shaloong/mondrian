//! Settlement of direct native children belongs to the invoking worker.

use std::io;
use std::process::{Child, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) enum ChildWaitError<E> {
    Checkpoint(E),
    Deadline,
    Observation(io::Error),
    Settlement(io::Error),
}

// The guard is created before invoking caller code. A checkpoint panic must not
// orphan the foreign process while the catalog worker unwinds.
struct SettlementGuard<'a> {
    child: &'a mut Child,
    settled: bool,
}

impl SettlementGuard<'_> {
    fn terminate(&mut self) -> io::Result<()> {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            self.settled = true;
            return Ok(());
        }
        if let Err(error) = self.child.kill() {
            // Distinguish a concurrent natural exit from an OS refusal to
            // terminate. Waiting on a still-running, unkillable child would
            // strand catalog destruction indefinitely.
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.settled = true;
                return Ok(());
            }
            return Err(error);
        }
        self.child.wait()?;
        self.settled = true;
        Ok(())
    }
}

impl Drop for SettlementGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.terminate();
        }
    }
}

pub(crate) fn wait_for_child<E>(
    child: &mut Child,
    deadline: Instant,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<ExitStatus, ChildWaitError<E>> {
    let mut owner = SettlementGuard { child, settled: false };
    loop {
        if let Err(error) = checkpoint() {
            owner.terminate().map_err(ChildWaitError::Settlement)?;
            return Err(ChildWaitError::Checkpoint(error));
        }
        match owner.child.try_wait() {
            Ok(Some(status)) => {
                owner.settled = true;
                return Ok(status);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                owner.terminate().map_err(ChildWaitError::Settlement)?;
                return Err(ChildWaitError::Deadline);
            }
            Err(error) => {
                owner.terminate().map_err(ChildWaitError::Settlement)?;
                return Err(ChildWaitError::Observation(error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const READY: &str = "MONDRIAN_CHILD_SETTLEMENT_TEST_READY";

    fn fixture_name(name: &str) -> String {
        let (_, module) = module_path!().split_once("::").expect("test module");
        format!("{module}::{name}")
    }

    struct Probe {
        child: Child,
        ready: PathBuf,
    }

    impl Probe {
        fn start() -> Self {
            let ready = std::env::temp_dir().join(format!(
                "mondrian-child-settlement-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let child = Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--ignored",
                    "--exact",
                    &fixture_name("sleeping_child"),
                    "--nocapture",
                ])
                .env(READY, &ready)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("direct child");
            let mut probe = Self { child, ready };
            let deadline = Instant::now() + Duration::from_secs(5);
            while !probe.ready.is_file() {
                assert!(probe.child.try_wait().expect("observe child").is_none());
                assert!(
                    Instant::now() < deadline,
                    "child must enter fixture before cancellation"
                );
                thread::sleep(Duration::from_millis(5));
            }
            probe
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_file(&self.ready);
        }
    }

    #[test]
    #[ignore = "direct-child fixture, entered only by its owning test"]
    fn sleeping_child() {
        fs::write(std::env::var_os(READY).expect("readiness path"), b"entered").expect("readiness");
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn cancellation_settles_a_running_direct_child_before_native_deadline() {
        let mut probe = Probe::start();
        let started = Instant::now();
        let result = wait_for_child(
            &mut probe.child,
            started + Duration::from_millis(250),
            &mut || Err("owner closed"),
        );
        assert!(
            matches!(result, Err(ChildWaitError::Checkpoint("owner closed"))),
            "{result:?}"
        );
        assert!(probe.child.try_wait().expect("settled child").is_some());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn native_deadline_settles_a_running_direct_child() {
        let mut probe = Probe::start();
        let result = wait_for_child(
            &mut probe.child,
            Instant::now() + Duration::from_millis(30),
            &mut || Ok::<_, ()>(()),
        );
        assert!(
            matches!(result, Err(ChildWaitError::Deadline)),
            "{result:?}"
        );
        assert!(probe.child.try_wait().expect("settled child").is_some());
    }

    #[test]
    fn checkpoint_panic_settles_a_running_direct_child() {
        let mut probe = Probe::start();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = wait_for_child(
                &mut probe.child,
                Instant::now() + Duration::from_secs(30),
                &mut || -> Result<(), ()> { panic!("checkpoint panic") },
            );
        }));
        assert!(result.is_err());
        assert!(probe.child.try_wait().expect("settled child").is_some());
    }

    #[test]
    #[ignore = "direct-child exit fixture"]
    fn exiting_child() {
        std::process::exit(
            std::env::var("MONDRIAN_CHILD_TEST_EXIT").expect("exit").parse().expect("code"),
        );
    }

    #[test]
    fn successful_and_failed_child_exit_are_observed_and_reaped() {
        for code in [0, 7] {
            let mut child = Command::new(std::env::current_exe().expect("test executable"))
                .args(["--ignored", "--exact", &fixture_name("exiting_child")])
                .env("MONDRIAN_CHILD_TEST_EXIT", code.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("exit child");
            let status = wait_for_child(
                &mut child,
                Instant::now() + Duration::from_secs(5),
                &mut || Ok::<_, ()>(()),
            )
            .expect("exit observed");
            assert_eq!(status.code(), Some(code));
            assert_eq!(child.try_wait().expect("reaped child"), Some(status));
        }
    }
}
