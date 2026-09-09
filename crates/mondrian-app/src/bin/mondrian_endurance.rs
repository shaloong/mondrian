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
use mondrian_app::app::endurance_physical_machine::{
    EnduranceReferenceProviderRegistry, PhysicalEnduranceMachineFactory,
};
use mondrian_app::app::endurance_product_runtime::{
    run_product_endurance_campaign, EnduranceSurfaceDriverShutdownEvidence,
    EnduranceSurfaceRecoveryPump, EnduranceSurfaceReopenDriver, EnduranceSurfaceReopenRun,
    PreparedEnduranceMachinePhaseFactory, WindowEnduranceSurfaceReopenDriver,
};
use mondrian_app::app::endurance_run_request::PreparedEnduranceRunRequest;
use mondrian_app::app::AppState;
use mondrian_platform::SystemPlatformService;
use serde::Serialize;

const SELF_TEST_REPORT_SCHEMA_VERSION: u32 = 2;

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
    ffmpeg_capsule_closure: mondrian_media::QualifiedFfmpegShutdownReceipt,
}

struct MachineSurfaceDriver {
    inner: Option<WindowEnduranceSurfaceReopenDriver>,
    unavailable: Option<String>,
}

impl EnduranceSurfaceReopenDriver for MachineSurfaceDriver {
    fn reopen(
        &mut self,
        app_state: AppState,
        recovery_pump: EnduranceSurfaceRecoveryPump,
        cycle_index: u32,
        operation_id: String,
        timeout: Duration,
    ) -> EnduranceSurfaceReopenRun {
        match &mut self.inner {
            Some(driver) => {
                driver.reopen(app_state, recovery_pump, cycle_index, operation_id, timeout)
            }
            None => EnduranceSurfaceReopenRun {
                app_state,
                recovery_pump,
                window_receipt: None,
                result: Err(self
                    .unavailable
                    .clone()
                    .unwrap_or_else(|| "native Surface driver was not prepared".to_owned())),
            },
        }
    }

    fn shutdown(
        self,
    ) -> Result<EnduranceSurfaceDriverShutdownEvidence, EnduranceRunOwnerShutdownFailure> {
        match self.inner {
            Some(driver) => driver.shutdown(),
            None => Ok(EnduranceSurfaceDriverShutdownEvidence::NotApplicable),
        }
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
        ffmpeg_capsule_closure: factory
            .shutdown_ffmpeg_capsule_until(std::time::Instant::now() + Duration::from_secs(5)),
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
    if !report.ffmpeg_capsule_closure.all_resources_released() {
        bail!("preflight capsule closure was incomplete; raw evidence retained in the self-test report");
    }
    Ok(())
}

fn run_campaign(request_path: PathBuf) -> anyhow::Result<()> {
    let request = PreparedEnduranceRunRequest::load(&request_path)
        .with_context(|| format!("admit strict request {}", request_path.display()))?;
    let surface = match WindowEnduranceSurfaceReopenDriver::new() {
        Ok(driver) => MachineSurfaceDriver { inner: Some(driver), unavailable: None },
        Err(error) => MachineSurfaceDriver { inner: None, unavailable: Some(error.to_string()) },
    };
    let factory = match PreparedEnduranceMachinePhaseFactory::prepare(&request, |ffmpeg| {
        Ok(PhysicalEnduranceMachineFactory::new(
            ffmpeg,
            EnduranceReferenceProviderRegistry::platform(),
            surface.inner.as_ref(),
        ))
    }) {
        Ok(factory) => factory,
        Err(error) => {
            surface.shutdown().map_err(|failure| {
                anyhow::anyhow!("{error}; Surface shutdown: {}", failure.diagnostic())
            })?;
            return Err(error.into());
        }
    };
    let clock = Arc::new(SystemEnduranceCampaignClock::new());
    let platform = SystemPlatformService;
    let manifest = run_product_endurance_campaign(request, factory, surface, clock, &platform)?;
    println!("MONDRIAN_ENDURANCE_RUN_ID={}", manifest.run_id);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mode = std::env::args_os().nth(1);
    if mode.as_deref() == Some(OsStr::new("--internal-demux-worker-v2")) {
        if std::env::args_os().count() != 2 {
            bail!("unexpected native demux worker arguments");
        }
        return mondrian_media::run_preview_demux_worker();
    }
    if mode.as_deref() == Some(OsStr::new(mondrian_media::MEDIA_PROBE_WORKER_ARGUMENT)) {
        return mondrian_media::run_media_probe_worker();
    }
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
