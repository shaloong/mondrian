//! Explicit native BMX capsule exercise, separate from physical qualification.
#![cfg(all(windows, feature = "validation"))]

use anyhow::{ensure, Context, Result};
use mondrian_core::ExecutionCancellationToken;
use mondrian_media::{
    prepare_bmx_runtime, prepare_bmx_runtime_for_phase, run_supervised_command, ApprovedBmxCommand,
    ApprovedBmxTool, ApprovedProviderFile, SupervisedProcessPolicy, SupervisedStreamCapture,
};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn retained(path: PathBuf) -> Result<ApprovedProviderFile> {
    let mut file = OpenOptions::new().read(true).share_mode(1).open(&path)?;
    ensure!(
        file.metadata()?.len() <= 512 * 1024 * 1024,
        "bounded native fixture"
    );
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(ApprovedProviderFile::from_retained(
        path,
        hash.finalize().into(),
        file,
    ))
}

fn tools(directory: &Path) -> Result<[ApprovedProviderFile; 2]> {
    Ok([
        retained(directory.join("raw2bmx.exe"))?,
        retained(directory.join("mxf2raw.exe"))?,
    ])
}

fn policy(deadline: Instant) -> SupervisedProcessPolicy {
    SupervisedProcessPolicy {
        deadline: Some(deadline),
        stdout: SupervisedStreamCapture::Head { limit_bytes: 65536, reject_excess: true },
        stderr: SupervisedStreamCapture::Head { limit_bytes: 65536, reject_excess: true },
        ..Default::default()
    }
}

#[test]
#[ignore = "requires explicit official BMX executables, native capsule ACLs and create-only evidence path"]
fn native_bmx_capsule_keeps_owner_horizon_and_consumes_real_children() -> Result<()> {
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_BMX_TOOL_DIR").context("BMX fixture directory required")?,
    );
    let output = PathBuf::from(
        std::env::var_os("MONDRIAN_BMX_NATIVE_EVIDENCE_OUTPUT")
            .context("create-only native evidence path required")?,
    );
    ensure!(!output.exists(), "native evidence already exists");
    let token = ExecutionCancellationToken::new();
    let horizon = Instant::now() + Duration::from_secs(120);
    let preparation_deadline = Instant::now() + Duration::from_secs(5);
    let prepared = prepare_bmx_runtime_for_phase(
        tools(&directory)?,
        Some(Vec::new()),
        preparation_deadline,
        horizon,
        &token,
    )?
    .context("native BMX capsule admission unavailable")?;
    let handle = prepared.handle();
    let mut observations = Vec::new();
    let mut probes_after_preparation_deadline = false;
    let operation = (|| -> Result<()> {
        let mut expired = handle.command(ApprovedBmxTool::Raw2Bmx);
        expired.arg("-v");
        ensure!(
            run_supervised_command(&mut expired, None, policy(Instant::now()), &token).is_err(),
            "an already-expired probe started successfully"
        );
        drop(expired);
        // Actual prepared commands must retain their originally admitted phase
        // horizon after the distinct, shorter namespace preparation lease ends.
        std::thread::sleep(
            preparation_deadline.saturating_duration_since(Instant::now())
                + Duration::from_millis(5),
        );
        probes_after_preparation_deadline = Instant::now() >= preparation_deadline;
        for tool in [ApprovedBmxTool::Raw2Bmx, ApprovedBmxTool::Mxf2Raw] {
            let leaf = match tool {
                ApprovedBmxTool::Raw2Bmx => "raw2bmx.exe",
                ApprovedBmxTool::Mxf2Raw => "mxf2raw.exe",
            };
            let mut source_command = ApprovedBmxCommand::unapproved(&directory.join(leaf));
            source_command.arg("-v");
            let source = run_supervised_command(
                &mut source_command,
                None,
                policy((Instant::now() + Duration::from_secs(15)).min(horizon)),
                &token,
            )?;
            observations.push(serde_json::json!({
                "stage":"original_file_control","tool":format!("{tool:?}"),
                "status":source.status.code(),"stdout":String::from_utf8_lossy(&source.stdout),
                "stderr":String::from_utf8_lossy(&source.stderr),"cleanup":source.cleanup,
            }));
            ensure!(
                source.status.success() && source.cleanup.all_resources_released(),
                "original BMX version control failed"
            );
            let mut command = handle.command(tool);
            command.arg("-v");
            let result = run_supervised_command(
                &mut command,
                None,
                policy((Instant::now() + Duration::from_secs(15)).min(horizon)),
                &token,
            )?;
            let success = result.status.success()
                && result.cleanup.all_resources_released()
                && !result.stdout_truncated
                && !result.stderr_truncated
                && (!result.stdout.is_empty() || !result.stderr.is_empty())
                && result.stdout == source.stdout
                && result.stderr == source.stderr;
            observations.push(serde_json::json!({
                "stage":"sealed_capsule","tool":format!("{tool:?}"),"status":result.status.code(),
                "stdout":String::from_utf8_lossy(&result.stdout),
                "stderr":String::from_utf8_lossy(&result.stderr),"cleanup":result.cleanup,
                "original_file_control_cleanup":source.cleanup,
                "original_and_staged_version_bytes_equal":result.stdout==source.stdout && result.stderr==source.stderr,
            }));
            ensure!(success, "real BMX version probe or native closure failed");
        }
        token.cancel();
        let mut canceled = handle.command(ApprovedBmxTool::Mxf2Raw);
        canceled.arg("-v");
        ensure!(
            run_supervised_command(&mut canceled, None, policy(horizon), &token).is_err(),
            "canceled owner admitted another tool"
        );
        Ok(())
    })();
    drop(handle);
    let closure = prepared.close_until(horizon);
    let clean = closure.namespace_owned && closure.all_resources_released();
    let report = serde_json::json!({
        "schema_version":1,"qualified":false,
        "scope":"native BMX capsule/version/deadline/cancellation/consuming closure",
        "preparation_timeout_ms":5000,"owner_horizon_ms":120000,
        "probes_after_preparation_deadline":probes_after_preparation_deadline,
        "failure":operation.as_ref().err().map(ToString::to_string),
        "native_probes":observations,"runtime_closure":closure,
    });
    let mut file = OpenOptions::new().create_new(true).write(true).open(&output)?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    operation?;
    ensure!(
        clean,
        "native BMX runtime closure failed; raw evidence retained"
    );
    Ok(())
}

#[test]
#[ignore = "requires explicit official BMX files; tests a quarantined read-only owner, not qualification"]
fn native_bmx_live_borrow_cannot_claim_consuming_closure() -> Result<()> {
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_BMX_TOOL_DIR").context("BMX fixture directory required")?,
    );
    let mut output = std::env::var_os("MONDRIAN_BMX_NATIVE_EVIDENCE_OUTPUT")
        .context("native evidence path required")?;
    output.push(".live-borrow.json");
    let token = ExecutionCancellationToken::new();
    let horizon = Instant::now() + Duration::from_secs(30);
    // No private namespace is created for this intentional negative case.
    // Quarantined leases refer only to the existing read-only fixture files;
    // the native test process exit releases them without leaving a sealed Temp.
    let prepared = prepare_bmx_runtime(tools(&directory)?, None, horizon, &token)?
        .context("ordinary unqualified owner required for this negative case")?;
    let borrowed = prepared.handle();
    let closure = prepared.close_until(horizon);
    let rejected = !closure.commands_released
        && !closure.namespace_owned
        && !closure.file_leases_released
        && closure.outstanding_owner_error.is_some()
        && !closure.all_resources_released();
    drop(borrowed);
    let mut file = OpenOptions::new().create_new(true).write(true).open(PathBuf::from(output))?;
    serde_json::to_writer_pretty(
        &mut file,
        &serde_json::json!({
            "schema_version":1,"qualified":false,"expected_rejection":rejected,
            "scope":"live read-only borrow prevents consuming closure; no sealed namespace created",
            "runtime_closure":closure,
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    ensure!(rejected, "a live borrowed runtime was accepted as consumed");
    Ok(())
}
