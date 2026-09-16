#![cfg(target_os = "linux")]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::process::Command;

const POLICY: &str = "VK_LOADER_DISABLE_DYNAMIC_LIBRARY_UNLOADING";
const CHILD_OUTPUT: &str = "MONDRIAN_GRAPHICS_BOOTSTRAP_TEST_OUTPUT";

fn run_child(policy: Option<&OsStr>) -> (std::process::Output, Vec<String>) {
    let directory = tempfile::tempdir().expect("temporary child output");
    let path = directory.path().join("visits");
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .arg0("graphics probe with spaces")
        .args(["--ignored", "--exact", "bootstrap_child", "--nocapture"])
        .current_dir(directory.path())
        .env(CHILD_OUTPUT, &path)
        .env(
            "MONDRIAN_GRAPHICS_BOOTSTRAP_TEST_VALUE",
            OsString::from_vec(vec![0xff, 0xfe]),
        )
        .env_remove(POLICY);
    if let Some(value) = policy {
        command.env(POLICY, value);
    }
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("bootstrap child");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if child.try_wait().expect("child status").is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().expect("stop timed-out child");
            child.wait().expect("reap timed-out child");
            panic!("graphics bootstrap did not finish before its test deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("bootstrap child output");
    let visits = std::fs::read_to_string(path)
        .expect("child visit record")
        .lines()
        .map(str::to_owned)
        .collect();
    (output, visits)
}

#[test]
fn absent_policy_reexecutes_once_without_changing_process_identity() {
    let (output, visits) = run_child(None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        visits.len(),
        2,
        "the initial and reexecuted entries must both run"
    );
    assert_eq!(visits[0], visits[1], "exec must preserve the process PID");
}

#[test]
fn established_policy_does_not_reexecute() {
    let (output, visits) = run_child(Some(OsStr::new("1")));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(visits.len(), 1);
}

#[test]
fn conflicting_policy_is_rejected_without_reexecution() {
    for value in [
        OsString::from("0"),
        OsString::from(""),
        OsString::from_vec(vec![0xff]),
    ] {
        let (output, visits) = run_child(Some(&value));
        assert_eq!(output.status.code(), Some(42));
        assert_eq!(visits.len(), 1);
        assert!(String::from_utf8_lossy(&output.stderr).contains("ConflictingLoaderPolicy"));
    }
}

#[test]
#[ignore = "helper invoked only in an isolated process by the bootstrap tests"]
fn bootstrap_child() {
    use std::io::Write;

    let path = std::env::var_os(CHILD_OUTPUT).expect("isolated bootstrap child");
    {
        let mut record = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("visit file");
        writeln!(record, "{}", std::process::id()).expect("record PID");
    }
    if let Err(error) = mondrian_platform::prepare_graphics_process() {
        eprintln!("{error:?}");
        std::process::exit(42);
    }
    assert_eq!(std::env::var_os(POLICY).as_deref(), Some(OsStr::new("1")));
    assert_eq!(
        std::env::args_os().next().expect("argv[0]").as_bytes(),
        b"graphics probe with spaces"
    );
    assert_eq!(
        std::env::args_os().skip(1).collect::<Vec<_>>(),
        ["--ignored", "--exact", "bootstrap_child", "--nocapture"].map(OsString::from)
    );
    assert_eq!(
        std::env::var_os("MONDRIAN_GRAPHICS_BOOTSTRAP_TEST_VALUE")
            .expect("inherited environment")
            .as_bytes(),
        &[0xff, 0xfe]
    );
    assert_eq!(
        std::env::current_dir().expect("cwd"),
        std::path::Path::new(&path).parent().expect("parent")
    );
}
