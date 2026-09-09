//! One immutable, machine-plan-bound Broadcast program for physical and Export owners.
use std::sync::Arc;

use mondrian_broadcast::{AncillaryFrame, FrozenAncillaryProgram};
use mondrian_core::Rational;
#[cfg(any(windows, test))]
use mondrian_core::{FrameRounding, TimelineTime};

use super::endurance_machine_plan::EnduranceMachineFileBinding;

/// Exact source bytes, canonical interpretation and native source lease shared by all phases.
#[derive(Debug)]
pub struct PreparedEnduranceAncillaryProgram {
    sha256: String,
    program: FrozenAncillaryProgram,
    first_frame: u64,
    journals: mondrian_reference_output::NativeAncillaryJournalInventory,
    exports: std::sync::Mutex<
        Vec<(
            String,
            super::endurance_campaign::EnduranceAncillaryExportArtifact,
        )>,
    >,
    #[cfg(windows)]
    _lease: std::fs::File,
}

/// Explicit absence cannot be confused with an undeclared optional attachment.
#[derive(Debug, Clone, Default)]
pub(crate) enum EnduranceAncillaryAdmission {
    #[default]
    NotRequested,
    NotRun,
    Ready(Arc<PreparedEnduranceAncillaryProgram>),
}

impl EnduranceAncillaryAdmission {
    pub(crate) fn prepare(binding: Option<&EnduranceMachineFileBinding>) -> Result<Self, String> {
        let Some(binding) = binding else {
            return Ok(Self::NotRequested);
        };
        #[cfg(not(windows))]
        {
            let _ = binding;
            Ok(Self::NotRun)
        }
        #[cfg(windows)]
        {
            match std::fs::symlink_metadata(&binding.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Self::NotRun)
                }
                Err(error) => return Err(error.to_string()),
                Ok(_) => {}
            }
            let (lease, bytes, sha256) = super::endurance_source_inventory::read_bound_file(
                binding,
                8 * 1024 * 1024,
                "ancillary_program",
            )
            .map_err(|error| error.to_string())?;
            let program: FrozenAncillaryProgram =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            let first_frame = validate_program(&program)?;
            Ok(Self::Ready(Arc::new(PreparedEnduranceAncillaryProgram {
                sha256,
                program,
                first_frame,
                journals: Default::default(),
                exports: Default::default(),
                _lease: lease,
            })))
        }
    }
    pub(crate) fn program(&self) -> Option<&Arc<PreparedEnduranceAncillaryProgram>> {
        match self {
            Self::Ready(program) => Some(program),
            _ => None,
        }
    }
    pub(crate) fn missing(&self) -> bool {
        matches!(self, Self::NotRun)
    }
}

#[cfg(any(windows, test))]
fn validate_program(program: &FrozenAncillaryProgram) -> Result<u64, String> {
    program.validate().map_err(|error| error.to_string())?;
    let frame = program
        .source_start()
        .to_frame_position(program.output_frame_rate(), FrameRounding::Floor)
        .map_err(|error| error.to_string())?;
    if TimelineTime::from_frame_position(frame).map_err(|error| error.to_string())?
        != program.source_start()
    {
        return Err("ANC Timeline origin is not exactly on the program output grid".to_owned());
    }
    let first = u64::try_from(frame.frame).map_err(|_| "negative ANC origin".to_owned())?;
    first.checked_add(program.frame_count()).ok_or("ANC Timeline end overflow")?;
    Ok(first)
}

impl PreparedEnduranceAncillaryProgram {
    pub(crate) fn register_verified_export(
        &self,
        phase_id: &str,
        receipt: super::endurance_campaign::EnduranceAncillaryExportArtifact,
    ) -> Result<(), String> {
        let mut values = self.exports.lock().map_err(|_| "ANC Export inventory lock poisoned")?;
        if values.iter().filter(|(phase, _)| phase == phase_id).count() >= 256
            || values.iter().any(|(_, item)| item.artifact_id == receipt.artifact_id)
        {
            return Err("ANC verified Export inventory repeated or exceeds phase bound".to_owned());
        }
        values.push((phase_id.to_owned(), receipt));
        Ok(())
    }
    pub(crate) fn verified_exports(
        &self,
        phase_id: &str,
    ) -> Result<Vec<super::endurance_campaign::EnduranceAncillaryExportArtifact>, String> {
        let values = self.exports.lock().map_err(|_| "ANC Export inventory lock poisoned")?;
        Ok(values
            .iter()
            .filter(|(phase, _)| phase == phase_id)
            .map(|(_, item)| item.clone())
            .collect())
    }
    /// Consumed native wire owners for this exact phase, never inferred from filenames.
    pub fn closed_wire_journals(
        &self,
        phase_id: &str,
    ) -> Result<Vec<mondrian_reference_output::NativeAncillaryJournalReceipt>, String> {
        self.journals.closed_for_phase(phase_id)
    }
    pub(crate) fn journal_binding(
        &self,
        phase_id: &str,
    ) -> mondrian_reference_output::NativeAncillaryJournalBinding {
        mondrian_reference_output::NativeAncillaryJournalBinding {
            ancillary_program_sha256: self.sha256.clone(),
            phase_id: phase_id.to_owned(),
            inventory: self.journals.clone(),
        }
    }
    /// Hash of the same retained source bytes parsed into this program.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    /// Sole Broadcast interpretation; callers cannot replace it behind this identity.
    pub fn program(&self) -> &FrozenAncillaryProgram {
        &self.program
    }
    /// Exact physical grid binding; no caption-rate conversion is inferred.
    pub fn validate_rate(&self, rate: Rational) -> Result<(), String> {
        if self.program.output_frame_rate() != rate {
            return Err("ANC and physical output frame rates differ".to_owned());
        }
        Ok(())
    }
    /// Preflight every sparse packet and the owner marker before native scheduling.
    pub fn validate_wire(
        &self,
        signal: &mondrian_reference_output::ReferenceOutputSignal,
        correlation: &mondrian_broadcast::AncillaryWireCorrelation,
    ) -> Result<(), String> {
        self.validate_rate(signal.frame_rate)?;
        for frame in self.program.nonempty_frames() {
            let bound = correlation
                .frame(frame.frame_index(), frame.packets().to_vec())
                .map_err(|error| error.to_string())?;
            if bound.packets().len() > 64 {
                return Err(
                    "canonical ANC plus owner marker exceeds native 64-packet bound".to_owned(),
                );
            }
            for packet in bound.packets() {
                if packet.placement.space != mondrian_broadcast::AncillarySpace::Vanc
                    || packet.placement.field != mondrian_broadcast::AncillaryField::Progressive
                    || u32::from(packet.placement.horizontal_offset)
                        + packet.packet.component_words().len() as u32
                        > signal.width
                {
                    return Err(
                        "canonical ANC is outside the native progressive luma VANC row".to_owned(),
                    );
                }
            }
        }
        Ok(())
    }
    /// Place one canonical selection-relative inventory at an absolute Timeline frame.
    /// Frames outside the selected attachment range are explicitly empty, never looped.
    pub fn frame_at(&self, absolute: u64, rate: Rational) -> Result<AncillaryFrame, String> {
        self.validate_rate(rate)?;
        let relative = absolute.checked_sub(self.first_frame);
        let Some(relative) = relative.filter(|index| *index < self.program.frame_count()) else {
            return Ok(AncillaryFrame::empty(absolute));
        };
        let frame = self.program.frame(relative).map_err(|error| error.to_string())?;
        AncillaryFrame::new(absolute, frame.packets().to_vec()).map_err(|error| error.to_string())
    }
}

pub(crate) fn same_program(
    expected: Option<&Arc<PreparedEnduranceAncillaryProgram>>,
    actual: Option<&Arc<PreparedEnduranceAncillaryProgram>>,
) -> bool {
    match (expected, actual) {
        (None, None) => true,
        (Some(expected), Some(actual)) => Arc::ptr_eq(expected, actual),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonintegral_origin_and_noncanonical_program_fail_before_execution() {
        let program = FrozenAncillaryProgram::new(
            TimelineTime::new(1, 7).expect("time"),
            Rational::new(60000, 1001),
            2,
            vec![],
        )
        .expect("program");
        assert!(validate_program(&program).is_err());
        let program = FrozenAncillaryProgram::new(
            TimelineTime::new(1001, 60000).expect("time"),
            Rational::new(60000, 1001),
            2,
            vec![],
        )
        .expect("program");
        assert_eq!(validate_program(&program).expect("exact origin"), 1);
    }
    #[cfg(windows)]
    #[test]
    fn retained_shared_program_rejects_replacement_and_maps_absolute_frames_without_looping() {
        use mondrian_broadcast::{
            AncillaryField, AncillaryOrigin, AncillaryPacket, AncillaryPlacement, AncillarySpace,
            AncillaryValidationLevel, St291Type2Packet,
        };
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir().expect("root");
        let path = mondrian_assets::canonical_native_path(root.path())
            .expect("canonical")
            .join("anc.json");
        let frame = AncillaryFrame::new(
            0,
            vec![AncillaryPacket {
                placement: AncillaryPlacement::new(
                    AncillarySpace::Vanc,
                    AncillaryField::Progressive,
                    20,
                    0,
                )
                .expect("placement"),
                packet: St291Type2Packet::from_8bit_payload(0x45, 0x01, &[1, 2]).expect("packet"),
                origin: AncillaryOrigin::Derived,
                validation: AncillaryValidationLevel::Transport,
            }],
        )
        .expect("frame");
        let program = FrozenAncillaryProgram::new(
            TimelineTime::new(1001, 30000).expect("time"),
            Rational::new(60000, 1001),
            2,
            vec![frame],
        )
        .expect("program");
        let bytes = serde_json::to_vec(&program).expect("json");
        std::fs::write(&path, &bytes).expect("write");
        let binding = EnduranceMachineFileBinding {
            path: path.clone(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let admitted = EnduranceAncillaryAdmission::prepare(Some(&binding)).expect("admit");
        let owner = admitted.program().expect("ready");
        assert!(std::fs::write(&path, b"changed").is_err());
        let copy = Arc::clone(owner);
        assert!(same_program(Some(owner), Some(&copy)));
        assert!(!same_program(Some(owner), None));
        assert!(owner
            .frame_at(1, Rational::new(60000, 1001))
            .expect("before")
            .packets()
            .is_empty());
        assert_eq!(
            owner.frame_at(2, Rational::new(60000, 1001)).expect("selected").packets().len(),
            1
        );
        assert_eq!(
            owner.frame_at(2, Rational::new(60000, 1001)).expect("absolute").frame_index(),
            2
        );
        assert!(owner
            .frame_at(4, Rational::new(60000, 1001))
            .expect("after")
            .packets()
            .is_empty());
        assert!(owner
            .frame_at(u64::MAX, Rational::new(60000, 1001))
            .expect("far after")
            .packets()
            .is_empty());
        assert!(owner.frame_at(2, Rational::new(60, 1)).is_err());
        let canonical =
            owner.frame_at(2, Rational::new(60000, 1001)).expect("canonical pump input");
        let marker = mondrian_broadcast::AncillaryWireCorrelation::new(
            [7; 16],
            AncillaryPlacement::new(AncillarySpace::Vanc, AncillaryField::Progressive, 21, 0)
                .expect("marker placement"),
        )
        .expect("marker");
        let wire = marker.frame(2, canonical.packets().to_vec()).expect("canonical plus marker");
        assert_eq!(wire.packets().len(), 2);
        assert!(wire.packets().contains(&canonical.packets()[0]));
        let actual = wire
            .packets()
            .iter()
            .map(|packet| mondrian_broadcast::CapturedAncillaryPacket {
                placement: packet.placement,
                component_words: packet.packet.component_words(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            marker.captured_frame_index(&actual).expect("receive correlation"),
            2
        );
        let collision = mondrian_broadcast::AncillaryWireCorrelation::new(
            [8; 16],
            canonical.packets()[0].placement,
        )
        .expect("collision marker");
        assert!(collision.frame(2, canonical.packets().to_vec()).is_err());
        let receipt = super::super::endurance_campaign::EnduranceAncillaryExportArtifact {
            artifact_id: "endurance-export-test".to_owned(),
            verification_path: path.with_file_name("verified.json"),
            verification_sha256: "a".repeat(64),
        };
        owner
            .register_verified_export("phase-a", receipt.clone())
            .expect("joined receipt");
        assert_eq!(
            owner.verified_exports("phase-a").expect("phase"),
            vec![receipt.clone()]
        );
        assert!(owner.verified_exports("phase-b").expect("other phase").is_empty());
        assert!(owner.register_verified_export("phase-b", receipt).is_err());

        let wrong = EnduranceMachineFileBinding { sha256: "0".repeat(64), ..binding.clone() };
        assert!(EnduranceAncillaryAdmission::prepare(Some(&wrong)).is_err());
        drop(copy);
        drop(admitted);
        std::fs::remove_file(&path).expect("release lease");
        assert!(EnduranceAncillaryAdmission::prepare(Some(&binding)).expect("missing").missing());
    }
}
