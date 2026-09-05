//! Unified strict entrypoint for commercial endurance execution and preflight.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use mondrian_app::app::endurance_campaign::EnduranceRunOwnerShutdownFailure;
use mondrian_app::app::endurance_campaign::SystemEnduranceCampaignClock;
use mondrian_app::app::endurance_machine_factory::ContinuousExportEnduranceMachineFactory;
use mondrian_app::app::endurance_product_runtime::{
    run_product_endurance_campaign, EnduranceSurfaceDriverShutdownEvidence,
    EnduranceSurfaceRecoveryPump, EnduranceSurfaceReopenDriver, EnduranceSurfaceReopenRun,
    PreparedEnduranceMachinePhaseFactory,
};
use mondrian_app::app::endurance_run_request::PreparedEnduranceRunRequest;
use mondrian_app::app::AppState;
use mondrian_platform::SystemPlatformService;
use serde::Serialize;

const SELF_TEST_REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct EndurancePreflightSelfTestReport<'a> {
    schema_version: u32,
    qualifying: bool,
    scope: &'static str,
    limitation: &'static str,
    request_sha256: &'a str,
    machine_plan_sha256: &'a str,
    ffmpeg_toolchain_receipt_sha256: &'a str,
}

struct NoPhysicalSurfaceDriver;

impl EnduranceSurfaceReopenDriver for NoPhysicalSurfaceDriver {
    fn reopen(
        &mut self,
        _app_state: AppState,
        _recovery_pump: EnduranceSurfaceRecoveryPump,
        _cycle_index: u32,
        _operation_id: String,
        _timeout: Duration,
    ) -> EnduranceSurfaceReopenRun {
        panic!("Continuous Export-only factory cannot admit Surface recovery")
    }

    fn shutdown(
        self,
    ) -> Result<EnduranceSurfaceDriverShutdownEvidence, EnduranceRunOwnerShutdownFailure> {
        Ok(EnduranceSurfaceDriverShutdownEvidence::NotApplicable)
    }
}

fn strict_path(argument: &OsStr, label: &str) -> anyhow::Result<PathBuf> {
    let value = argument.to_str().with_context(|| format!("{label} is not valid UTF-8"))?;
    if value.is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(PathBuf::from(value))
}

fn prepare_factory(
    request: &PreparedEnduranceRunRequest,
) -> Result<
    PreparedEnduranceMachinePhaseFactory<ContinuousExportEnduranceMachineFactory>,
    mondrian_app::app::endurance_campaign::EnduranceCampaignError,
> {
    PreparedEnduranceMachinePhaseFactory::prepare(request, |ffmpeg| {
        Ok(ContinuousExportEnduranceMachineFactory::new(ffmpeg))
    })
}

fn write_self_test(request_path: PathBuf, report_path: PathBuf) -> anyhow::Result<()> {
    if report_path.exists() {
        bail!("self-test report already exists: {}", report_path.display());
    }
    let request = PreparedEnduranceRunRequest::load(&request_path)
        .with_context(|| format!("admit strict request {}", request_path.display()))?;
    let factory = prepare_factory(&request)?;
    let report = EndurancePreflightSelfTestReport {
        schema_version: SELF_TEST_REPORT_SCHEMA_VERSION,
        qualifying: false,
        scope: "strict-request-and-exact-ffmpeg-preflight-only",
        limitation:
            "does not create an App, execute a phase, exercise physical output, or qualify duration",
        request_sha256: request.request_sha256(),
        machine_plan_sha256: request.machine_plan().sha256(),
        ffmpeg_toolchain_receipt_sha256: factory.ffmpeg_toolchain_receipt_sha256(),
    };
    let bytes = serde_json::to_vec_pretty(&report)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&report_path)
        .with_context(|| format!("create self-test report {}", report_path.display()))?;
    output.write_all(&bytes)?;
    output.sync_all()?;
    println!("MONDRIAN_ENDURANCE_SELF_TEST={}", report_path.display());
    Ok(())
}

fn run_campaign(request_path: PathBuf) -> anyhow::Result<()> {
    let request = PreparedEnduranceRunRequest::load(&request_path)
        .with_context(|| format!("admit strict request {}", request_path.display()))?;
    let factory = prepare_factory(&request)?;
    let clock = Arc::new(SystemEnduranceCampaignClock::new());
    let platform = SystemPlatformService;
    let manifest = run_product_endurance_campaign(
        request,
        factory,
        NoPhysicalSurfaceDriver,
        clock,
        &platform,
    )?;
    println!("MONDRIAN_ENDURANCE_RUN_ID={}", manifest.run_id);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [request] => run_campaign(strict_path(request, "run request")?),
        [mode, request, report] if mode == "--self-test" => write_self_test(
            strict_path(request, "run request")?,
            strict_path(report, "self-test report")?,
        ),
        _ => bail!(
            "expected <run-request.json> or --self-test <run-request.json> <create-only-report.json>"
        ),
    }
}
