//! Concrete three-phase composition over exact machine-local physical providers.

use std::collections::HashMap;
use std::sync::Arc;

use mondrian_media::{probe_realtime_audio_output_contract, RealtimeAudioOutputDeviceSelection};
use mondrian_platform::{EndurancePhaseKind, EndurancePhaseRequirement};
use mondrian_reference_output::{
    ReferenceOutputAdapter, ReferenceOutputAdapterError, ReferenceOutputProvider,
    ReferenceOutputRuntimeAvailability,
};

use super::endurance_campaign::EnduranceCampaignError;
use super::endurance_ffmpeg_toolchain::PreparedEnduranceFfmpegToolchain;
use super::endurance_machine_factory::{
    build_machine_phase, direct_directory, exact_export_plan, regular_direct_file,
    validate_phase_binding, PreparedMachineReference,
};
use super::endurance_machine_plan::PreparedCommercialEnduranceMachinePlan;
use super::endurance_product_runtime::{
    FreshEndurancePhaseBuild, FreshEndurancePhaseFactory, WindowEnduranceSurfaceReopenDriver,
};
use super::endurance_workload::{
    EndurancePreStartCapability as Capability, EndurancePreStartCapabilityInventory,
    PreparedEndurancePhaseStart, PreparedEnduranceWorkload,
};

type ProviderConstructor =
    Box<dyn FnMut() -> Result<Box<dyn ReferenceOutputAdapter>, ReferenceOutputAdapterError>>;

/// Process-local constructors for compiled native Reference Output adapters.
///
/// Registration does not acquire a device. Every admission constructs a fresh
/// adapter and performs real discovery. An absent implementation stays absent;
/// no unavailable or simulated bridge can advertise a physical capability.
#[derive(Default)]
pub struct EnduranceReferenceProviderRegistry {
    constructors: HashMap<ReferenceOutputProvider, ProviderConstructor>,
    #[cfg(windows)]
    native_aja_registered: bool,
    #[cfg(windows)]
    native_decklink_registered: bool,
}

impl EnduranceReferenceProviderRegistry {
    /// Register native implementations compiled for this platform. DLL loading
    /// and actual driver/device discovery are deferred until phase admission.
    pub fn platform() -> Self {
        #[cfg(windows)]
        {
            let mut registry = Self {
                native_aja_registered: true,
                native_decklink_registered: true,
                ..Self::default()
            };
            registry.constructors.insert(
                ReferenceOutputProvider::AjaNtv2,
                Box::new(|| {
                    mondrian_reference_output::AjaReferenceOutputAdapter::load_packaged()
                        .map(|adapter| Box::new(adapter) as Box<dyn ReferenceOutputAdapter>)
                }),
            );
            registry.constructors.insert(
                ReferenceOutputProvider::DeckLink,
                Box::new(|| {
                    mondrian_reference_output::DeckLinkReferenceOutputAdapter::load_packaged()
                        .map(|adapter| Box::new(adapter) as Box<dyn ReferenceOutputAdapter>)
                }),
            );
            registry
        }
        #[cfg(not(windows))]
        Self::default()
    }
    /// Register exactly one constructor for a physical provider family.
    pub fn register(
        &mut self,
        provider: ReferenceOutputProvider,
        constructor: impl FnMut() -> Result<Box<dyn ReferenceOutputAdapter>, ReferenceOutputAdapterError>
            + 'static,
    ) -> Result<(), ReferenceOutputAdapterError> {
        if provider == ReferenceOutputProvider::Simulated
            || self.constructors.contains_key(&provider)
        {
            return Err(ReferenceOutputAdapterError::ProviderMismatch);
        }
        self.constructors.insert(provider, Box::new(constructor));
        Ok(())
    }

    fn construct(
        &mut self,
        provider: ReferenceOutputProvider,
    ) -> Result<Box<dyn ReferenceOutputAdapter>, ReferenceOutputAdapterError> {
        self.constructors.get_mut(&provider).ok_or_else(|| {
            ReferenceOutputAdapterError::DriverMissing {
                detail: format!("no compiled native {provider:?} bridge is registered"),
            }
        })?()
    }
}

struct PreparedPhysicalAdmission {
    phase_id: String,
    workload_id: String,
    kind: EndurancePhaseKind,
    reference: Option<PreparedMachineReference>,
}

/// Concrete machine factory for Playback/Reference, repeated Export and recovery.
///
/// The exact physical adapter discovered during admission transfers to the
/// phase App. Audio uses an exact stable device selection and the production
/// negotiator. Provider loss after admission is a startup failure, never a
/// synthetic fallback or a successful physical campaign.
pub struct PhysicalEnduranceMachineFactory {
    machine_plan_sha256: String,
    providers: EnduranceReferenceProviderRegistry,
    surface_prepared: bool,
    prepared: Option<PreparedPhysicalAdmission>,
    diagnostics: Vec<String>,
}

impl PhysicalEnduranceMachineFactory {
    /// Bind the FFmpeg authority and compiled providers to an optional actual
    /// process-local Window recovery owner created before campaign admission.
    pub fn new(
        ffmpeg: &PreparedEnduranceFfmpegToolchain,
        providers: EnduranceReferenceProviderRegistry,
        surface: Option<&WindowEnduranceSurfaceReopenDriver>,
    ) -> Self {
        Self {
            machine_plan_sha256: ffmpeg.machine_plan_sha256().to_owned(),
            providers,
            surface_prepared: surface.is_some(),
            prepared: None,
            diagnostics: Vec::new(),
        }
    }

    /// Missing provider/device/fixture observations from the most recent admission.
    pub fn admission_diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    fn validate_plan(
        &self,
        plan: &PreparedCommercialEnduranceMachinePlan,
    ) -> Result<(), EnduranceCampaignError> {
        if self.machine_plan_sha256 != plan.sha256() {
            return Err(EnduranceCampaignError::Runtime(
                "physical factory belongs to another machine plan".to_owned(),
            ));
        }
        Ok(())
    }

    fn prepare_reference(
        &mut self,
        plan: &PreparedCommercialEnduranceMachinePlan,
        capabilities: &mut Vec<Capability>,
        phase_id: &str,
    ) -> Option<PreparedMachineReference> {
        let planned = &plan.plan().reference_output;
        let result = (|| {
            if planned.open_request.ancillary_policy.requires_readback()
                && planned.wire_readback.is_none()
            {
                return Err(ReferenceOutputAdapterError::ModeUnsupported);
            }
            let (mut adapter, wire_correlation): (Box<dyn ReferenceOutputAdapter>, _) =
                if let Some(wire) = &planned.wire_readback {
                    #[cfg(windows)]
                    {
                        use mondrian_broadcast::{
                            AncillaryField, AncillaryPlacement, AncillarySpace,
                            AncillaryWireCorrelation,
                        };
                        use sha2::{Digest, Sha256};
                        static GENERATION: std::sync::atomic::AtomicU64 =
                            std::sync::atomic::AtomicU64::new(0);
                        let registered = match planned.provider {
                            ReferenceOutputProvider::AjaNtv2 => {
                                self.providers.native_aja_registered
                            }
                            ReferenceOutputProvider::DeckLink => {
                                self.providers.native_decklink_registered
                            }
                            ReferenceOutputProvider::Simulated => false,
                        };
                        if !registered {
                            return Err(ReferenceOutputAdapterError::ProviderMismatch);
                        }
                        let mut hash = Sha256::new();
                        hash.update(plan.sha256().as_bytes());
                        hash.update(
                            GENERATION
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                .to_be_bytes(),
                        );
                        hash.update(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_err(|error| ReferenceOutputAdapterError::Vendor {
                                    operation: "wire-nonce",
                                    detail: error.to_string(),
                                })?
                                .as_nanos()
                                .to_be_bytes(),
                        );
                        let digest = hash.finalize();
                        let mut nonce = [0u8; 16];
                        nonce.copy_from_slice(&digest[..16]);
                        let placement = AncillaryPlacement::new(
                            AncillarySpace::Vanc,
                            AncillaryField::Progressive,
                            wire.marker_line,
                            wire.marker_horizontal_offset,
                        )
                        .map_err(|error| {
                            ReferenceOutputAdapterError::Vendor {
                                operation: "wire-marker",
                                detail: error.to_string(),
                            }
                        })?;
                        let correlation =
                            AncillaryWireCorrelation::new(nonce, placement).map_err(|error| {
                                ReferenceOutputAdapterError::Vendor {
                                    operation: "wire-marker",
                                    detail: error.to_string(),
                                }
                            })?;
                        if let Some(owner) = plan.ancillary_program() {
                            owner
                                .validate_wire(&planned.open_request.signal, &correlation)
                                .map_err(|detail| ReferenceOutputAdapterError::Vendor {
                                    operation: "wire-program-admission",
                                    detail,
                                })?;
                        }
                        let adapter: Box<dyn ReferenceOutputAdapter> = match planned.provider {
                            ReferenceOutputProvider::AjaNtv2 => Box::new(
                                mondrian_reference_output::AjaReferenceOutputAdapter::load_packaged()?
                                    .with_wire_readback(mondrian_reference_output::AjaWireReadbackConfiguration {
                                        device_id: wire.device_id.clone(),
                                        device_generation: wire.device_generation,
                                        signal: planned.open_request.signal.clone(),
                                        correlation: correlation.clone(),
                                        receipt_directory: wire.receipt_directory.clone(),
                                        maximum_receipt_bytes: wire.maximum_receipt_bytes,
                                        program_journal: plan.ancillary_program().map(|owner|owner.journal_binding(phase_id)),
                                    })?,
                            ),
                            ReferenceOutputProvider::DeckLink => Box::new(
                                mondrian_reference_output::DeckLinkReferenceOutputAdapter::load_packaged()?
                                    .with_wire_readback(mondrian_reference_output::DeckLinkWireReadbackConfiguration {
                                        device_id: wire.device_id.clone(),
                                        device_generation: wire.device_generation,
                                        signal: planned.open_request.signal.clone(),
                                        correlation: correlation.clone(),
                                        receipt_directory: wire.receipt_directory.clone(),
                                        maximum_receipt_bytes: wire.maximum_receipt_bytes,
                                        program_journal: plan.ancillary_program().map(|owner|owner.journal_binding(phase_id)),
                                    })?,
                            ),
                            ReferenceOutputProvider::Simulated => return Err(ReferenceOutputAdapterError::ProviderMismatch),
                        };
                        (adapter, Some(correlation))
                    }
                    #[cfg(not(windows))]
                    {
                        let _ = wire;
                        return Err(ReferenceOutputAdapterError::ModeUnsupported);
                    }
                } else {
                    (self.providers.construct(planned.provider)?, None)
                };
            let devices = adapter.discover()?;
            let evidence = adapter.evidence();
            if evidence.provider != planned.provider
                || !evidence.hardware_backed
                || evidence.availability != ReferenceOutputRuntimeAvailability::Available
                || evidence.adapter_version.trim().is_empty()
                || evidence.sdk_version.as_ref().is_none_or(|version| version.trim().is_empty())
                || evidence.driver_version.as_ref().is_none_or(|version| version.trim().is_empty())
            {
                return Err(ReferenceOutputAdapterError::ReadbackMismatch);
            }
            let mut matching = devices.into_iter().filter(|device| {
                device.id == planned.device_id
                    && device.provider == planned.provider
                    && device.generation == planned.device_generation
            });
            let device = matching.next().ok_or(ReferenceOutputAdapterError::DeviceUnavailable)?;
            if matching.next().is_some() {
                return Err(ReferenceOutputAdapterError::ReadbackMismatch);
            }
            device.admit(&planned.open_request.production_request())?;
            capabilities.push(Capability::PhysicalReferenceProviderPrepared);
            match adapter.preflight_reference_lock(&device)? {
                Some(true) => capabilities.push(Capability::ExternalReferenceSignalPreflight),
                Some(false) => self
                    .diagnostics
                    .push("selected physical Reference device is not externally locked".to_owned()),
                None => self.diagnostics.push(
                    "selected provider cannot observe external lock before output open".to_owned(),
                ),
            }
            Ok(PreparedMachineReference { adapter, device, wire_correlation })
        })();
        match result {
            Ok(reference) => Some(reference),
            Err(error) => {
                self.diagnostics.push(format!("physical Reference preflight: {error}"));
                None
            }
        }
    }
}

impl FreshEndurancePhaseFactory for PhysicalEnduranceMachineFactory {
    fn pre_start_capability_inventory(
        &mut self,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
    ) -> Result<EndurancePreStartCapabilityInventory, EnduranceCampaignError> {
        self.validate_plan(machine_plan)?;
        validate_phase_binding(requirement, workload)?;
        self.prepared = None;
        self.diagnostics.clear();
        let mut capabilities = Vec::new();
        let project_declared = regular_direct_file(&machine_plan.plan().project.project.path)
            && regular_direct_file(&machine_plan.plan().project.external_source_inventory.path);
        if !project_declared {
            self.diagnostics
                .push("exact canonical Project or external-source inventory is absent".to_owned());
        }
        let physical = requirement.kind != EndurancePhaseKind::ContinuousExport;
        let exporting = requirement.kind != EndurancePhaseKind::PlaybackReference;
        if exporting {
            let export = exact_export_plan(machine_plan, &requirement.phase_id)?;
            capabilities.push(Capability::IndependentExportVerifierPrepared);
            if project_declared
                && regular_direct_file(&export.preset.path)
                && export
                    .broadcast_qc
                    .as_ref()
                    .is_none_or(|binding| regular_direct_file(&binding.path))
                && direct_directory(&export.output_directory)
            {
                capabilities.push(Capability::FrozenExportFixtureDeclared);
            } else {
                self.diagnostics.push(
                    "phase Export fixture, preset, QC profile or output directory is absent"
                        .to_owned(),
                );
            }
        }
        let reference = if physical {
            if project_declared {
                capabilities.push(Capability::TimelinePlaybackFixtureDeclared);
            }
            let audio = &machine_plan.plan().audio;
            let selection =
                RealtimeAudioOutputDeviceSelection::Specific { device_id: audio.device_id.clone() };
            match probe_realtime_audio_output_contract(
                &selection,
                audio.sample_rate_hz,
                audio.channel_layout,
            ) {
                Ok(evidence)
                    if evidence.device_id == audio.device_id
                        && evidence.selection == selection
                        && evidence.contract.sample_rate == audio.sample_rate_hz
                        && evidence.contract.channel_layout == audio.channel_layout =>
                {
                    capabilities.push(Capability::AudioOutputDevicePrepared);
                }
                Ok(_) => self.diagnostics.push(
                    "physical Audio negotiation returned mismatched device/rate/layout".to_owned(),
                ),
                Err(error) => self.diagnostics.push(format!("physical Audio preflight: {error}")),
            }
            self.prepare_reference(machine_plan, &mut capabilities, &requirement.phase_id)
        } else {
            None
        };
        if requirement.kind == EndurancePhaseKind::ConcurrentRecovery {
            if self.surface_prepared {
                capabilities.push(Capability::SurfaceEventLoopPrepared);
            } else {
                self.diagnostics
                    .push("native Window recovery EventLoop is unavailable".to_owned());
            }
            // Exact seek bounds are checked against the loaded canonical Sequence
            // before playback starts; these are reachable production operations.
            if !machine_plan.plan().recovery_seek_targets.is_empty() {
                capabilities.push(Capability::SeekRecoveryPrepared);
            }
            capabilities.push(Capability::ExportCancelRetryPrepared);
            capabilities.push(Capability::CachePressurePrepared);
        }
        let inventory = EndurancePreStartCapabilityInventory::new(capabilities);
        if workload.prepare_start(&inventory).is_ok() {
            self.prepared = Some(PreparedPhysicalAdmission {
                phase_id: requirement.phase_id.clone(),
                workload_id: workload.workload_id().to_owned(),
                kind: requirement.kind,
                reference,
            });
        }
        for detail in &self.diagnostics {
            tracing::info!(phase = %requirement.phase_id, %detail, "endurance pre-start observation");
        }
        Ok(inventory)
    }

    fn build_phase(
        &mut self,
        machine_plan: Arc<PreparedCommercialEnduranceMachinePlan>,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        prepared_start: PreparedEndurancePhaseStart,
    ) -> FreshEndurancePhaseBuild {
        if let Err(error) = self
            .validate_plan(&machine_plan)
            .and_then(|()| validate_phase_binding(requirement, workload))
        {
            return FreshEndurancePhaseBuild::rejected(error.to_string());
        }
        let Some(prepared) = self.prepared.take() else {
            return FreshEndurancePhaseBuild::rejected(
                "physical machine build has no retained exact admission",
            );
        };
        if prepared.phase_id != requirement.phase_id
            || prepared.workload_id != workload.workload_id()
            || prepared.kind != requirement.kind
            || prepared_start.phase_id() != requirement.phase_id
            || prepared_start.workload_id() != workload.workload_id()
            || prepared_start.kind() != requirement.kind
        {
            return FreshEndurancePhaseBuild::rejected(
                "physical machine admission token does not match the exact phase/workload",
            );
        }
        build_machine_phase(machine_plan, requirement, prepared.reference)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_reference_output::{
        ReferenceOutputAdapterSession, ReferenceOutputDeviceDescriptor, ReferenceOutputMode,
        ReferenceOutputOpenRequest, ReferenceOutputProviderEvidence,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct PreflightAdapter {
        evidence: ReferenceOutputProviderEvidence,
        devices: Vec<ReferenceOutputDeviceDescriptor>,
        locked: Option<bool>,
        opens: Arc<AtomicUsize>,
    }
    impl ReferenceOutputAdapter for PreflightAdapter {
        fn evidence(&self) -> &ReferenceOutputProviderEvidence {
            &self.evidence
        }
        fn discover(
            &mut self,
        ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
            Ok(self.devices.clone())
        }
        fn preflight_reference_lock(
            &mut self,
            _: &ReferenceOutputDeviceDescriptor,
        ) -> Result<Option<bool>, ReferenceOutputAdapterError> {
            Ok(self.locked)
        }
        fn open(
            &mut self,
            _: &ReferenceOutputDeviceDescriptor,
            _: &ReferenceOutputOpenRequest,
        ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Err(ReferenceOutputAdapterError::SessionStopped)
        }
    }
    fn plan() -> (tempfile::TempDir, PreparedCommercialEnduranceMachinePlan) {
        let temporary = tempfile::tempdir().expect("machine directory");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let profile = serde_json::from_slice(
            &std::fs::read(root.join("tests/validation/commercial-endurance-qualification.json"))
                .expect("profile"),
        )
        .expect("parse profile");
        let path = temporary.path().join("plan.json");
        super::super::endurance_machine_plan::write_test_machine_plan(
            &path,
            temporary.path(),
            &profile,
            24,
        );
        let plan = PreparedCommercialEnduranceMachinePlan::load(&path, &profile, 24).expect("plan");
        (temporary, plan)
    }
    fn adapter(plan: &PreparedCommercialEnduranceMachinePlan) -> PreflightAdapter {
        let planned = &plan.plan().reference_output;
        PreflightAdapter {
            evidence: ReferenceOutputProviderEvidence {
                provider: planned.provider,
                adapter_version: "test-only-preflight".to_owned(),
                sdk_version: Some("1".to_owned()),
                driver_version: Some("1".to_owned()),
                hardware_backed: true,
                availability: ReferenceOutputRuntimeAvailability::Available,
            },
            devices: vec![ReferenceOutputDeviceDescriptor {
                id: planned.device_id.clone(),
                provider: planned.provider,
                display_name: "test-only discovery".to_owned(),
                generation: planned.device_generation,
                modes: vec![ReferenceOutputMode {
                    signal: planned.open_request.signal.clone(),
                    supports_hdr_signal: true,
                    supports_static_hdr_metadata: true,
                    supports_reference_status: true,
                    supports_ancillary: true,
                    supports_ancillary_readback: true,
                }],
            }],
            locked: Some(true),
            opens: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn factory(
        plan: &PreparedCommercialEnduranceMachinePlan,
        adapter: Option<PreflightAdapter>,
    ) -> PhysicalEnduranceMachineFactory {
        let mut providers = EnduranceReferenceProviderRegistry::default();
        if let Some(adapter) = adapter {
            providers
                .register(plan.plan().reference_output.provider, move || {
                    Ok(Box::new(adapter.clone()))
                })
                .expect("register");
        }
        PhysicalEnduranceMachineFactory {
            machine_plan_sha256: plan.sha256().to_owned(),
            providers,
            surface_prepared: false,
            prepared: None,
            diagnostics: Vec::new(),
        }
    }
    #[test]
    fn physical_preflight_observes_exact_discovery_and_lock_without_opening_session() {
        let (_temporary, plan) = plan();
        let adapter = adapter(&plan);
        let opens = Arc::clone(&adapter.opens);
        let mut factory = factory(&plan, Some(adapter));
        let mut capabilities = Vec::new();
        assert!(factory.prepare_reference(&plan, &mut capabilities, "test-phase").is_some());
        assert_eq!(
            capabilities,
            vec![
                Capability::PhysicalReferenceProviderPrepared,
                Capability::ExternalReferenceSignalPreflight
            ]
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn physical_preflight_rejects_spoofed_stale_ambiguous_or_unversioned_discovery() {
        let (_temporary, plan) = plan();
        for mutation in 0..9 {
            let mut adapter = adapter(&plan);
            match mutation {
                0 => adapter.evidence.hardware_backed = false,
                1 => adapter.evidence.provider = ReferenceOutputProvider::Simulated,
                2 => adapter.evidence.sdk_version = None,
                3 => adapter.evidence.driver_version = Some(" ".to_owned()),
                4 => adapter.devices[0].generation += 1,
                5 => adapter.devices.push(adapter.devices[0].clone()),
                6 => adapter.devices[0].modes.clear(),
                7 => adapter.evidence.availability = ReferenceOutputRuntimeAvailability::NoDevices,
                _ => adapter.evidence.adapter_version.clear(),
            }
            let mut factory = factory(&plan, Some(adapter));
            let mut capabilities = Vec::new();
            assert!(
                factory.prepare_reference(&plan, &mut capabilities, "test-phase").is_none(),
                "mutation {mutation}"
            );
            assert!(!capabilities.contains(&Capability::ExternalReferenceSignalPreflight));
            assert!(!factory.admission_diagnostics().is_empty());
        }
    }
    #[test]
    fn physical_preflight_never_promotes_unknown_or_unlocked_reference() {
        let (_temporary, plan) = plan();
        for locked in [None, Some(false)] {
            let mut adapter = adapter(&plan);
            adapter.locked = locked;
            let mut factory = factory(&plan, Some(adapter));
            let mut capabilities = Vec::new();
            assert!(factory.prepare_reference(&plan, &mut capabilities, "test-phase").is_some());
            assert_eq!(
                capabilities,
                vec![Capability::PhysicalReferenceProviderPrepared]
            );
            assert!(!factory.admission_diagnostics().is_empty());
        }
        let mut factory = factory(&plan, None);
        assert!(factory.prepare_reference(&plan, &mut Vec::new(), "test-phase").is_none());
    }
    #[test]
    fn physical_registry_rejects_simulation_and_duplicate_replacement() {
        let mut registry = EnduranceReferenceProviderRegistry::default();
        assert!(registry
            .register(ReferenceOutputProvider::Simulated, || Err(
                ReferenceOutputAdapterError::NoDevices
            ))
            .is_err());
        registry
            .register(ReferenceOutputProvider::DeckLink, || {
                Err(ReferenceOutputAdapterError::NoDevices)
            })
            .expect("first registration");
        assert!(registry
            .register(ReferenceOutputProvider::DeckLink, || Err(
                ReferenceOutputAdapterError::NoDevices
            ))
            .is_err());
    }
}
