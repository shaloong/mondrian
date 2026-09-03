//! Exact Continuous Export composition for the commercial endurance runtime.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mondrian_export::IndependentExportArtifactPolicy;
use mondrian_platform::{EndurancePhaseKind, EndurancePhaseRequirement};

use super::endurance_campaign::EnduranceCampaignError;
use super::endurance_export::FrozenRepeatedExportRequest;
use super::endurance_ffmpeg_toolchain::PreparedEnduranceFfmpegToolchain;
use super::endurance_machine_plan::{
    EnduranceMachineExportPlan, PreparedCommercialEnduranceMachinePlan,
};
use super::endurance_product_runtime::{
    FreshEndurancePhase, FreshEndurancePhaseBuild, FreshEndurancePhaseFactory,
    PreparedEndurancePhaseAuthority,
};
use super::endurance_source_inventory::PreparedEnduranceSourceInventory;
use super::endurance_workload::{
    EndurancePreStartCapability, EndurancePreStartCapabilityInventory, PreparedEndurancePhaseStart,
    PreparedEnduranceWorkload,
};
use super::AppState;

/// Machine factory for the hardware-independent Continuous Export phase.
///
/// Construction is possible only from the exact FFmpeg receipt supplied by
/// `PreparedEnduranceMachinePhaseFactory::prepare`. Playback/Reference and
/// Concurrent Recovery remain `NotRun` until a physical factory composes their
/// Audio, Reference, external-lock, and recovery prerequisites.
pub struct ContinuousExportEnduranceMachineFactory {
    machine_plan_sha256: String,
}

impl ContinuousExportEnduranceMachineFactory {
    /// Bind this factory to the machine plan that installed the exact FFmpeg closure.
    pub fn new(ffmpeg: &PreparedEnduranceFfmpegToolchain) -> Self {
        Self {
            machine_plan_sha256: ffmpeg.machine_plan_sha256().to_owned(),
        }
    }

    #[cfg(test)]
    pub(crate) fn test(machine_plan: &PreparedCommercialEnduranceMachinePlan) -> Self {
        Self {
            machine_plan_sha256: machine_plan.sha256().to_owned(),
        }
    }

    fn validate_plan(
        &self,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
    ) -> Result<(), EnduranceCampaignError> {
        if self.machine_plan_sha256 != machine_plan.sha256() {
            return Err(factory_error(
                "Continuous Export factory belongs to a different machine plan",
            ));
        }
        Ok(())
    }
}

impl FreshEndurancePhaseFactory for ContinuousExportEnduranceMachineFactory {
    fn pre_start_capability_inventory(
        &mut self,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
    ) -> Result<EndurancePreStartCapabilityInventory, EnduranceCampaignError> {
        self.validate_plan(machine_plan)?;
        validate_phase_binding(requirement, workload)?;
        if requirement.kind != EndurancePhaseKind::ContinuousExport {
            return Ok(EndurancePreStartCapabilityInventory::default());
        }
        let export = exact_export_plan(machine_plan, &requirement.phase_id)?;
        let declared = regular_direct_file(&machine_plan.plan().project.project.path)
            && regular_direct_file(&machine_plan.plan().project.external_source_inventory.path)
            && regular_direct_file(&export.preset.path)
            && export
                .broadcast_qc
                .as_ref()
                .is_none_or(|binding| regular_direct_file(&binding.path))
            && direct_directory(&export.output_directory);
        let mut capabilities = vec![EndurancePreStartCapability::IndependentExportVerifierPrepared];
        if declared {
            capabilities.push(EndurancePreStartCapability::FrozenExportFixtureDeclared);
        }
        Ok(EndurancePreStartCapabilityInventory::new(capabilities))
    }

    fn build_phase(
        &mut self,
        machine_plan: Arc<PreparedCommercialEnduranceMachinePlan>,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        prepared_start: PreparedEndurancePhaseStart,
    ) -> FreshEndurancePhaseBuild {
        if let Err(error) = self.validate_plan(machine_plan.as_ref()) {
            return FreshEndurancePhaseBuild::rejected(error.to_string());
        }
        if let Err(error) = validate_phase_binding(requirement, workload) {
            return FreshEndurancePhaseBuild::rejected(error.to_string());
        }
        if requirement.kind != EndurancePhaseKind::ContinuousExport
            || prepared_start.phase_id() != requirement.phase_id
            || prepared_start.workload_id() != workload.workload_id()
            || prepared_start.kind() != requirement.kind
        {
            return FreshEndurancePhaseBuild::rejected(
                "Continuous Export factory token does not match the exact phase/workload",
            );
        }
        let export = match exact_export_plan(machine_plan.as_ref(), &requirement.phase_id) {
            Ok(export) => export.clone(),
            Err(error) => return FreshEndurancePhaseBuild::rejected(error.to_string()),
        };
        let verification_policy = match IndependentExportArtifactPolicy::new(
            export.maximum_artifact_bytes,
            Duration::from_millis(export.decode_timeout_ms),
        ) {
            Ok(policy) => policy,
            Err(error) => {
                return FreshEndurancePhaseBuild::rejected(format!(
                    "Continuous Export verification policy is invalid: {error}"
                ));
            }
        };

        let mut app = AppState::new();
        let project = match app.open_endurance_project_fixture(machine_plan.as_ref()) {
            Ok(project) => project,
            Err(error) => {
                return FreshEndurancePhaseBuild::failed(
                    app,
                    format!("install exact Continuous Export Project: {error}"),
                );
            }
        };
        let sources = match PreparedEnduranceSourceInventory::prepare(
            &app,
            machine_plan.as_ref(),
            &project,
        ) {
            Ok(sources) => sources,
            Err(error) => {
                return FreshEndurancePhaseBuild::failed(
                    app,
                    format!("prepare exact Continuous Export sources: {error}"),
                );
            }
        };
        let authority =
            match PreparedEndurancePhaseAuthority::new(&app, Arc::clone(&machine_plan), sources) {
                Ok(authority) => authority,
                Err(error) => {
                    return FreshEndurancePhaseBuild::failed(
                        app,
                        format!("bind exact Continuous Export source authority: {error}"),
                    );
                }
            };
        let preset = match authority.source_inventory().export_preset(&requirement.phase_id) {
            Some(preset) => preset.preset().clone(),
            None => {
                return FreshEndurancePhaseBuild::failed_with_authority(
                    app,
                    authority,
                    "prepared source inventory omitted the Continuous Export preset",
                );
            }
        };
        let broadcast_qc = authority
            .source_inventory()
            .broadcast_qc_profile(&requirement.phase_id)
            .map(|profile| profile.profile().clone());
        let request = FrozenRepeatedExportRequest {
            preset,
            sequence_id: Some(export.sequence_id),
            range: export.range.timeline_range(),
            output_directory: export.output_directory,
            artifact_prefix: export.artifact_prefix,
            broadcast_qc,
            verification_policy,
        };
        FreshEndurancePhaseBuild::ready(FreshEndurancePhase::continuous_export(
            app, authority, request,
        ))
    }
}

fn validate_phase_binding(
    requirement: &EndurancePhaseRequirement,
    workload: &PreparedEnduranceWorkload,
) -> Result<(), EnduranceCampaignError> {
    if requirement.phase_id != workload.phase_id() || requirement.kind != workload.kind() {
        return Err(factory_error(
            "machine factory received a mismatched phase/workload binding",
        ));
    }
    Ok(())
}

fn exact_export_plan<'a>(
    machine_plan: &'a PreparedCommercialEnduranceMachinePlan,
    phase_id: &str,
) -> Result<&'a EnduranceMachineExportPlan, EnduranceCampaignError> {
    machine_plan
        .plan()
        .exports
        .iter()
        .find(|export| export.phase_id == phase_id)
        .ok_or_else(|| factory_error("machine plan has no exact Continuous Export phase contract"))
}

fn regular_direct_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

fn direct_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
}

fn factory_error(detail: impl Into<String>) -> EnduranceCampaignError {
    EnduranceCampaignError::Runtime(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn profile() -> mondrian_platform::EnduranceQualificationProfile {
        serde_json::from_slice(
            &fs::read(root().join("tests/validation/commercial-endurance-qualification.json"))
                .expect("read profile"),
        )
        .expect("parse profile")
    }

    #[test]
    fn prestart_admits_only_reachable_continuous_export_declarations() {
        let temporary = tempfile::tempdir().expect("temporary machine factory");
        let plan_path = temporary.path().join("machine-plan.json");
        let profile = profile();
        super::super::endurance_machine_plan::write_test_machine_plan(
            &plan_path,
            temporary.path(),
            &profile,
            24,
        );
        let plan = PreparedCommercialEnduranceMachinePlan::load(&plan_path, &profile, 24)
            .expect("prepare test plan");
        let requirement = profile
            .phases
            .iter()
            .find(|phase| phase.kind == EndurancePhaseKind::ContinuousExport)
            .expect("Continuous Export requirement");
        let workload = PreparedEnduranceWorkload::load(
            requirement,
            &root().join("tests/validation/endurance-workloads/continuous-export-v1.json"),
        )
        .expect("prepare Continuous Export workload");
        let mut factory = ContinuousExportEnduranceMachineFactory::test(&plan);

        let missing = factory
            .pre_start_capability_inventory(&plan, requirement, &workload)
            .expect("observe missing declarations");
        assert!(workload.prepare_start(&missing).is_err());

        fs::write(&plan.plan().project.project.path, b"project").expect("write Project binding");
        fs::write(
            &plan.plan().project.external_source_inventory.path,
            b"inventory",
        )
        .expect("write inventory binding");
        let export = exact_export_plan(&plan, &requirement.phase_id).expect("Export plan");
        fs::write(&export.preset.path, b"preset").expect("write preset binding");
        fs::create_dir(&export.output_directory).expect("create output directory");

        let ready = factory
            .pre_start_capability_inventory(&plan, requirement, &workload)
            .expect("observe reachable declarations");
        let token = workload.prepare_start(&ready).expect("admit exact declarations");
        assert_eq!(token.phase_id(), requirement.phase_id);
        assert_eq!(token.kind(), EndurancePhaseKind::ContinuousExport);
    }
}
