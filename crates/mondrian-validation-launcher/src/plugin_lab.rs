//! Windows-only test process supervision. This is not native ABI attestation.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{windows::Job, FileBinding, LaunchError};

/// Exact frozen executable and create-only evidence paths for one libtest group.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestLaunchPlan {
    /// Protocol version, currently one.
    pub schema_version: u32,
    /// The externally selected executable and full SHA-256.
    pub executable: FileBinding,
    /// Literal argv, without shell interpretation.
    pub arguments: Vec<String>,
    /// Existing absolute working directory.
    pub working_directory: std::path::PathBuf,
    /// New stdout file; native children never inherit a drain pipe.
    pub stdout_path: std::path::PathBuf,
    /// New stderr file.
    pub stderr_path: std::path::PathBuf,
    /// Original process-tree deadline, including admission.
    pub deadline_ms: u64,
    /// New outer-owner report path.
    pub report_path: std::path::PathBuf,
}

/// Raw process-tree outcome, independent of libtest's own assertions.
#[derive(Serialize)]
pub struct TestLaunchReport {
    /// Observed parent terminal exit status.
    pub exit_code: Option<i32>,
    /// Original process deadline expired.
    pub deadline_exceeded: bool,
    /// Parent exited with a live native descendant; always a failed gate.
    pub descendants_outlived_parent: bool,
    /// Kernel accounting proved no owned native processes remain.
    pub descendants_reaped: bool,
    /// Admission, observation or termination failures.
    pub errors: Vec<String>,
}

/// Spawn suspended, attach to the existing kill-on-close Job, then resume.
/// Environment values are inherited from the invoking lab controller.
pub fn launch(plan: &TestLaunchPlan) -> Result<TestLaunchReport, LaunchError> {
    let started = Instant::now();
    if plan.schema_version != 1 || plan.deadline_ms == 0 || plan.deadline_ms > 86_400_000 {
        return Err(LaunchError(
            "invalid libtest launch version/deadline".into(),
        ));
    }
    for path in [
        &plan.executable.path,
        &plan.working_directory,
        &plan.stdout_path,
        &plan.stderr_path,
        &plan.report_path,
    ] {
        if !path.is_absolute() {
            return Err(LaunchError("libtest paths must be absolute".into()));
        }
    }
    // Deny replacement/deletion throughout native execution; the outer lab also
    // retains every selected helper/plugin file under its own read lease.
    let mut image = OpenOptions::new().read(true).share_mode(1).open(&plan.executable.path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = image.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != plan.executable.sha256.to_ascii_lowercase() {
        return Err(LaunchError("frozen libtest image digest mismatch".into()));
    }
    let stdout = OpenOptions::new().write(true).create_new(true).open(&plan.stdout_path)?;
    let stderr = OpenOptions::new().write(true).create_new(true).open(&plan.stderr_path)?;
    let deadline = started + Duration::from_millis(plan.deadline_ms);
    if Instant::now() >= deadline {
        return Err(LaunchError("libtest deadline expired before spawn".into()));
    }
    let job = Job::new()?;
    let mut child = Command::new(&plan.executable.path)
        .args(&plan.arguments)
        .current_dir(&plan.working_directory)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .creation_flags(
            windows_sys::Win32::System::Threading::DETACHED_PROCESS
                | windows_sys::Win32::System::Threading::CREATE_SUSPENDED,
        )
        .spawn()?;
    let mut report = TestLaunchReport {
        exit_code: None,
        deadline_exceeded: false,
        descendants_outlived_parent: false,
        descendants_reaped: false,
        errors: Vec::new(),
    };
    if let Err(error) = job.assign_and_resume(&child) {
        report.errors.push(error.to_string());
        // Assignment may have failed before the Job acquired this suspended
        // child. Kill that exact retained object as well as any acquired tree.
        if let Err(error) = child.kill() {
            report.errors.push(error.to_string());
        }
    } else {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // An exit observed after the original bound is not proof
                    // of an on-time completion; do not accept a late poll.
                    report.deadline_exceeded = Instant::now() >= deadline;
                    report.exit_code = status.code();
                    match job.active() {
                        Ok(active) => report.descendants_outlived_parent = active != 0,
                        Err(error) => report.errors.push(error.to_string()),
                    }
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(None) => {
                    report.deadline_exceeded = true;
                    break;
                }
                Err(error) => {
                    report.errors.push(error.to_string());
                    break;
                }
            }
        }
    }
    // Termination is also required after successful parent exit. A libtest
    // summary cannot prove that its plugin workers have settled.
    if let Err(error) = job.terminate() {
        report.errors.push(error.to_string());
    }
    let cleanup_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if report.exit_code.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => report.exit_code = status.code(),
                Ok(None) => {}
                Err(error) => {
                    report.errors.push(error.to_string());
                    break;
                }
            }
        }
        match job.active() {
            Ok(0) if child.try_wait()?.is_some() => {
                report.descendants_reaped = true;
                break;
            }
            Ok(_) if Instant::now() < cleanup_deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(_) => break,
            Err(error) => {
                report.errors.push(error.to_string());
                break;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(directory: &std::path::Path, fixture: &str, deadline_ms: u64) -> TestLaunchPlan {
        let path = std::env::current_exe().expect("libtest executable");
        let sha256 = format!(
            "{:x}",
            Sha256::digest(std::fs::read(&path).expect("test image"))
        );
        TestLaunchPlan {
            schema_version: 1,
            executable: FileBinding { path, sha256 },
            arguments: vec![
                format!("plugin_lab::tests::{fixture}"),
                "--exact".into(),
                "--ignored".into(),
                "--nocapture".into(),
            ],
            working_directory: directory.to_owned(),
            stdout_path: directory.join("stdout.log"),
            stderr_path: directory.join("stderr.log"),
            deadline_ms,
            report_path: directory.join("owner.json"),
        }
    }

    #[test]
    fn owner_accepts_normal_parent_exit_and_proves_empty_job() {
        let directory = tempfile::tempdir().expect("evidence directory");
        let report = launch(&plan(directory.path(), "passing_child", 10_000)).expect("launch");
        assert_eq!(report.exit_code, Some(0));
        assert!(!report.deadline_exceeded);
        assert!(!report.descendants_outlived_parent);
        assert!(report.descendants_reaped);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }

    #[test]
    fn owner_refuses_successful_parent_with_live_descendant_and_settles_job() {
        let directory = tempfile::tempdir().expect("evidence directory");
        let report = launch(&plan(directory.path(), "orphaning_child", 10_000)).expect("launch");
        assert_eq!(report.exit_code, Some(0));
        assert!(!report.deadline_exceeded);
        assert!(report.descendants_outlived_parent);
        assert!(report.descendants_reaped);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(std::fs::read_to_string(directory.path().join("stdout.log"))
            .expect("log")
            .contains("OWNED_DESCENDANT_READY="));
    }

    #[test]
    fn owner_deadline_terminates_parent_and_ready_grandchild() {
        let directory = tempfile::tempdir().expect("evidence directory");
        let started = Instant::now();
        let report =
            launch(&plan(directory.path(), "nested_hanging_child", 2_000)).expect("launch");
        assert!(report.deadline_exceeded);
        assert!(report.descendants_reaped);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(started.elapsed() < Duration::from_secs(8));
        assert!(std::fs::read_to_string(directory.path().join("stdout.log"))
            .expect("log")
            .contains("OWNED_DESCENDANT_READY="));
    }

    #[test]
    fn owner_rejects_changed_image_before_spawn_or_output_creation() {
        let directory = tempfile::tempdir().expect("evidence directory");
        let mut request = plan(directory.path(), "passing_child", 10_000);
        request.executable.sha256 = "0".repeat(64);
        assert!(launch(&request).is_err());
        assert!(!request.stdout_path.exists());
        assert!(!request.stderr_path.exists());
    }

    #[test]
    #[ignore = "owned direct-child fixture"]
    fn passing_child() {}

    #[test]
    #[ignore = "owned sleeping descendant fixture"]
    fn sleeping_child() {
        if let Some(path) = std::env::var_os("MONDRIAN_LAB_TEST_READY_FILE") {
            std::fs::write(path, b"ready").expect("publish real child readiness");
        }
        std::thread::sleep(Duration::from_secs(60));
    }

    #[test]
    #[ignore = "owned parent-with-live-child fixture"]
    #[expect(
        clippy::zombie_processes,
        reason = "this negative fixture must leave a live descendant for the outer Job to detect and settle"
    )]
    fn orphaning_child() {
        let directory = tempfile::tempdir().expect("child readiness");
        let readiness = directory.path().join("ready");
        let child = Command::new(std::env::current_exe().expect("libtest executable"))
            .args(["plugin_lab::tests::sleeping_child", "--exact", "--ignored"])
            .env("MONDRIAN_LAB_TEST_READY_FILE", &readiness)
            .creation_flags(windows_sys::Win32::System::Threading::DETACHED_PROCESS)
            .spawn()
            .expect("actual descendant");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !readiness.exists() {
            // The real outer Job owns every descendant on success and failure.
            // A readiness failure must not wait indefinitely on a failed kill.
            assert!(Instant::now() < deadline, "actual descendant did not enter");
            std::thread::sleep(Duration::from_millis(5));
        }
        println!("OWNED_DESCENDANT_READY={}", child.id());
        // The tested outer Job must detect and settle this still-live child.
    }

    #[test]
    #[ignore = "owned hanging parent-and-grandchild fixture"]
    fn nested_hanging_child() {
        orphaning_child();
        std::thread::sleep(Duration::from_secs(60));
    }
}
