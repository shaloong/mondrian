#![cfg(target_os = "macos")]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_CASE: &str = "MONDRIAN_MACOS_GRAPHICS_BOOTSTRAP_TEST_CASE";

fn run_child(case: &str) {
    // Native task role is process-wide and cannot be reset to unspecified by
    // the public setter. Each case owns a fresh process with no product owners.
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--ignored", "--exact", "bootstrap_child", "--nocapture"])
        .env(CHILD_CASE, case)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("isolated native bootstrap child");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().expect("native child status").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop timed-out bootstrap child");
            child.wait().expect("reap timed-out bootstrap child");
            panic!("native bootstrap exceeded its original test deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("native child output");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn unspecified_role_becomes_default_without_reexecution() {
    run_child("unspecified");
}

#[test]
fn existing_background_application_role_is_preserved() {
    run_child("background");
}

#[test]
#[ignore = "helper invoked only in a fresh process by the native bootstrap tests"]
fn bootstrap_child() {
    let case = std::env::var(CHILD_CASE).expect("isolated bootstrap case");
    let initial =
        mondrian_platform::observe_macos_playback_task_role().expect("native initial role");
    assert_eq!(
        initial, 0,
        "fresh CLI task must begin with unspecified role"
    );
    let expected = match case.as_str() {
        "unspecified" => 7,
        "background" => {
            unsafe extern "C" {
                static mach_task_self_: u32;
                fn task_policy_set(task: u32, flavor: u32, policy: *const i32, count: u32) -> i32;
            }
            // Public TASK_BACKGROUND_APPLICATION is 2. Only this disposable
            // child's role changes; the parent and other tests are unaffected.
            let background = 2;
            let result = unsafe { task_policy_set(mach_task_self_, 1, &background, 1) };
            assert_eq!(result, 0, "native background role setup");
            background
        }
        _ => panic!("unknown bootstrap case"),
    };
    let pid = std::process::id();
    for _ in 0..2 {
        mondrian_platform::prepare_graphics_process().expect("native graphics entry policy");
        assert_eq!(
            std::process::id(),
            pid,
            "native admission must not reexecute"
        );
        assert_eq!(
            mondrian_platform::observe_macos_playback_task_role().expect("native admitted role"),
            expected,
        );
    }
}
