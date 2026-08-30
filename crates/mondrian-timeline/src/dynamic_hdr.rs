//! Sequence-owned Dynamic HDR Program authoring.
//!
//! Dynamic metadata describes the final Program Output, not an input Clip.
//! This Module owns exact-time shot ranges, immutable analysis provenance,
//! delivery intent, strong references, range projection, and identity forking.

use mondrian_core::{
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, AuthoringList,
    DynamicHdrMetadataFamily, DynamicHdrProgramId, DynamicHdrShotId, DynamicHdrShotMetadata,
    DynamicHdrStandard, FramePosition, FrameRounding, MondrianError, Rational, Result,
    TimelineTime, TimelineTimeRange,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Maximum Dynamic HDR Programs retained by one Sequence.
pub const MAX_DYNAMIC_HDR_PROGRAMS: usize = 8;
/// Maximum authored shots retained by one Dynamic HDR Program.
pub const MAX_DYNAMIC_HDR_SHOTS: usize = 4096;
/// Maximum metadata-analysis adapter identity length.
pub const MAX_DYNAMIC_HDR_ADAPTER_IDENTITY_BYTES: usize = 256;

/// Sequence-level dynamic-metadata delivery intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum DynamicHdrDeliveryIntent {
    /// Publish no dynamic metadata. Rendered HDR may still carry separately
    /// authored static HDR metadata.
    #[default]
    Omit,
    /// Preserve an entire source file byte-for-byte.
    ///
    /// This is not Smart Render and never falls back to rendered output.
    PreserveSourceExact {
        /// Metadata family that the frozen source probe must detect.
        family: DynamicHdrMetadataFamily,
    },
    /// Regenerate from one analyzed final-Program metadata definition through
    /// a qualified runtime Adapter.
    Remake {
        /// Strong identity of the analyzed Program to project for delivery.
        program_id: DynamicHdrProgramId,
    },
}

impl AuthoringFootprint for DynamicHdrDeliveryIntent {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        if let Self::PreserveSourceExact { family } = self {
            collector.collect(family)?;
        }
        Ok(())
    }
}

/// Immutable provenance for one complete final-picture analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicHdrAnalysisProvenance {
    /// Stable Adapter product identity.
    pub adapter_id: String,
    /// Exact Adapter/algorithm version.
    pub adapter_version: String,
    /// Metadata schema or technical-specification identity.
    pub metadata_schema: String,
    /// Complete Prepared Visual Program author fingerprint analyzed by the
    /// Adapter. Any later picture edit makes this Program stale.
    pub visual_author_fingerprint: [u8; 32],
    /// SHA-256 of the complete canonical analyzed metadata payload.
    pub canonical_metadata_sha256: [u8; 32],
}

impl DynamicHdrAnalysisProvenance {
    fn validate(&self) -> Result<()> {
        for (label, value) in [
            ("adapter_id", self.adapter_id.as_str()),
            ("adapter_version", self.adapter_version.as_str()),
            ("metadata_schema", self.metadata_schema.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > MAX_DYNAMIC_HDR_ADAPTER_IDENTITY_BYTES {
                return Err(dynamic_hdr_error(format!(
                    "Dynamic HDR {label} must contain 1..={MAX_DYNAMIC_HDR_ADAPTER_IDENTITY_BYTES} bytes"
                )));
            }
        }
        if self.visual_author_fingerprint == [0; 32] || self.canonical_metadata_sha256 == [0; 32] {
            return Err(dynamic_hdr_error(
                "Dynamic HDR analysis requires non-zero picture and metadata fingerprints",
            ));
        }
        Ok(())
    }
}

impl AuthoringFootprint for DynamicHdrAnalysisProvenance {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.adapter_id)?;
        collector.collect(&self.adapter_version)?;
        collector.collect(&self.metadata_schema)
    }
}

/// One exact final-Program shot or per-frame event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicHdrShot {
    /// Stable shot identity within its owning Sequence.
    pub id: DynamicHdrShotId,
    /// Human-readable shot label.
    pub name: String,
    /// Exact Sequence-local half-open range.
    pub range: TimelineTimeRange,
    /// Format-specific metadata for this exact final-picture interval.
    pub metadata: DynamicHdrShotMetadata,
}

impl DynamicHdrShot {
    /// Construct one authored shot with a fresh stable identity.
    pub fn new(
        name: impl Into<String>,
        range: TimelineTimeRange,
        metadata: DynamicHdrShotMetadata,
    ) -> Self {
        Self {
            id: DynamicHdrShotId::new(),
            name: name.into(),
            range,
            metadata,
        }
    }
}

impl AuthoringFootprint for DynamicHdrShot {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.name)?;
        collector.collect(&self.range.start)?;
        collector.collect(&self.range.duration)?;
        collector.collect(&self.metadata)
    }
}

/// One complete analyzed Dynamic HDR Program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicHdrProgram {
    /// Stable Program identity within its owning Sequence.
    pub id: DynamicHdrProgramId,
    /// Human-readable analyzed Program label.
    pub name: String,
    /// Exact metadata standard and public version/profile identity.
    pub standard: DynamicHdrStandard,
    /// Immutable analysis and final-picture identity evidence.
    pub provenance: DynamicHdrAnalysisProvenance,
    /// Ordered, contiguous exact-time metadata intervals.
    pub shots: AuthoringList<DynamicHdrShot>,
}

impl DynamicHdrProgram {
    /// Construct an analyzed Program with a fresh stable identity.
    pub fn new(
        name: impl Into<String>,
        standard: DynamicHdrStandard,
        provenance: DynamicHdrAnalysisProvenance,
        shots: impl IntoIterator<Item = DynamicHdrShot>,
    ) -> Self {
        Self {
            id: DynamicHdrProgramId::new(),
            name: name.into(),
            standard,
            provenance,
            shots: AuthoringList::from_iter(shots),
        }
    }

    fn validate(&self, frame_rate: Rational) -> Result<()> {
        if self.name.trim().is_empty() || self.name.len() > 256 {
            return Err(dynamic_hdr_error(
                "Dynamic HDR Program name must contain 1..=256 bytes",
            ));
        }
        self.standard.validate()?;
        self.provenance.validate()?;
        if self.shots.is_empty() || self.shots.len() > MAX_DYNAMIC_HDR_SHOTS {
            return Err(dynamic_hdr_error(format!(
                "Dynamic HDR Program must contain 1..={MAX_DYNAMIC_HDR_SHOTS} shots"
            )));
        }
        let mut shot_ids = HashSet::with_capacity(self.shots.len());
        let mut previous_end = None;
        for shot in &self.shots {
            if !shot_ids.insert(shot.id) {
                return Err(dynamic_hdr_error(format!(
                    "duplicate Dynamic HDR Shot identity {}",
                    shot.id
                )));
            }
            if shot.name.trim().is_empty() || shot.name.len() > 256 || shot.range.is_empty() {
                return Err(dynamic_hdr_error(format!(
                    "Dynamic HDR Shot {} has an empty name or range",
                    shot.id
                )));
            }
            validate_frame_grid_boundary(shot.range.start, frame_rate)?;
            validate_frame_grid_boundary(shot.range.end().map_err(time_error)?, frame_rate)?;
            if let Some(end) = previous_end
                && shot.range.start != end
            {
                return Err(dynamic_hdr_error(
                    "Dynamic HDR shot ranges must be strictly ordered, contiguous, and non-overlapping",
                ));
            }
            previous_end = Some(shot.range.end().map_err(time_error)?);
            match (&self.standard, &shot.metadata) {
                (
                    DynamicHdrStandard::St2094_40Application4 { application_version },
                    DynamicHdrShotMetadata::St2094_40Application4(metadata),
                ) => metadata.validate(*application_version)?,
                (
                    DynamicHdrStandard::DolbyVision { cm_version, .. },
                    DynamicHdrShotMetadata::DolbyVision(metadata),
                ) => metadata.validate(cm_version)?,
                _ => {
                    return Err(dynamic_hdr_error(
                        "Dynamic HDR shot metadata does not match its Program standard",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl AuthoringFootprint for DynamicHdrProgram {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.name)?;
        collector.collect(&self.standard)?;
        collector.collect(&self.provenance)?;
        collector.collect(&self.shots)
    }
}

/// Closed mutation accepted by [`DynamicHdrAuthorState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case", deny_unknown_fields)]
pub enum DynamicHdrAuthorEdit {
    /// Replace the explicit Sequence delivery policy.
    SetDeliveryIntent {
        /// New delivery policy.
        intent: DynamicHdrDeliveryIntent,
    },
    /// Install or replace one qualified analyzed Program by stable identity.
    InstallAnalyzedProgram {
        /// Complete analyzed Program candidate.
        program: DynamicHdrProgram,
    },
    /// Remove one unreferenced analyzed Program.
    RemoveProgram {
        /// Program identity to remove.
        program_id: DynamicHdrProgramId,
    },
}

/// Sequence-owned Dynamic HDR Program catalog and delivery policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DynamicHdrAuthorState {
    delivery_intent: DynamicHdrDeliveryIntent,
    programs: AuthoringList<DynamicHdrProgram>,
}

impl DynamicHdrAuthorState {
    /// Current delivery intent.
    pub const fn delivery_intent(&self) -> &DynamicHdrDeliveryIntent {
        &self.delivery_intent
    }

    /// Ordered analyzed Program definitions.
    pub fn programs(&self) -> &[DynamicHdrProgram] {
        &self.programs
    }

    /// Resolve one strong Program identity.
    pub fn program(&self, id: DynamicHdrProgramId) -> Option<&DynamicHdrProgram> {
        self.programs.iter().find(|program| program.id == id)
    }

    /// Apply one bounded mutation and atomically validate the resulting state.
    pub fn apply(&mut self, edit: DynamicHdrAuthorEdit, frame_rate: Rational) -> Result<bool> {
        let mut candidate = self.clone();
        let changed = match edit {
            DynamicHdrAuthorEdit::SetDeliveryIntent { intent } => {
                if candidate.delivery_intent == intent {
                    false
                } else {
                    candidate.delivery_intent = intent;
                    true
                }
            }
            DynamicHdrAuthorEdit::InstallAnalyzedProgram { program } => {
                if let Some(existing) =
                    candidate.programs.iter_mut().find(|existing| existing.id == program.id)
                {
                    if *existing == program {
                        false
                    } else {
                        *existing = program;
                        true
                    }
                } else {
                    if candidate.programs.len() >= MAX_DYNAMIC_HDR_PROGRAMS {
                        return Err(dynamic_hdr_error(format!(
                            "Sequence reached the {MAX_DYNAMIC_HDR_PROGRAMS}-Program Dynamic HDR limit"
                        )));
                    }
                    candidate.programs.push(program);
                    true
                }
            }
            DynamicHdrAuthorEdit::RemoveProgram { program_id } => {
                if matches!(
                    candidate.delivery_intent,
                    DynamicHdrDeliveryIntent::Remake { program_id: referenced } if referenced == program_id
                ) {
                    return Err(dynamic_hdr_error(
                        "cannot remove the Dynamic HDR Program referenced by Remake intent",
                    ));
                }
                let before = candidate.programs.len();
                candidate.programs.retain(|program| program.id != program_id);
                before != candidate.programs.len()
            }
        };
        if !changed {
            return Ok(false);
        }
        candidate.validate(frame_rate)?;
        *self = candidate;
        Ok(true)
    }

    /// Validate identities, standard rows, exact shot geometry, and strong references.
    pub fn validate(&self, frame_rate: Rational) -> Result<()> {
        if self.programs.len() > MAX_DYNAMIC_HDR_PROGRAMS {
            return Err(dynamic_hdr_error(format!(
                "Sequence exceeds the {MAX_DYNAMIC_HDR_PROGRAMS}-Program Dynamic HDR limit"
            )));
        }
        let mut program_ids = HashSet::with_capacity(self.programs.len());
        for program in &self.programs {
            if !program_ids.insert(program.id) {
                return Err(dynamic_hdr_error(format!(
                    "duplicate Dynamic HDR Program identity {}",
                    program.id
                )));
            }
            program.validate(frame_rate)?;
        }
        match &self.delivery_intent {
            DynamicHdrDeliveryIntent::Omit => {}
            DynamicHdrDeliveryIntent::PreserveSourceExact { .. } => {}
            DynamicHdrDeliveryIntent::Remake { program_id } => {
                if !program_ids.contains(program_id) {
                    return Err(dynamic_hdr_error(format!(
                        "Remake intent references missing Dynamic HDR Program {program_id}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Project one analyzed Program onto an exact delivery range.
    ///
    /// The returned shot ranges are clipped and rebased to delivery-local zero.
    /// Stale picture analysis, gaps, overflow, or an uncovered delivery edge
    /// fail closed before any tool or encoder is started.
    pub fn project_delivery_range(
        &self,
        program_id: DynamicHdrProgramId,
        delivery_range: TimelineTimeRange,
        current_visual_author_fingerprint: [u8; 32],
    ) -> Result<PreparedDynamicHdrProgram> {
        if delivery_range.is_empty() {
            return Err(dynamic_hdr_error("Dynamic HDR delivery range is empty"));
        }
        let program = self.program(program_id).ok_or_else(|| {
            dynamic_hdr_error(format!("Dynamic HDR Program {program_id} does not exist"))
        })?;
        if program.provenance.visual_author_fingerprint != current_visual_author_fingerprint {
            return Err(dynamic_hdr_error(
                "Dynamic HDR analysis is stale for the current final Program picture",
            ));
        }
        let delivery_end = delivery_range.end().map_err(time_error)?;
        let mut projected = Vec::new();
        for shot in &program.shots {
            let shot_end = shot.range.end().map_err(time_error)?;
            let start = shot.range.start.max(delivery_range.start);
            let end = shot_end.min(delivery_end);
            if start >= end {
                continue;
            }
            projected.push(PreparedDynamicHdrShot {
                source_shot_id: shot.id,
                name: shot.name.clone(),
                range: TimelineTimeRange::new(
                    start.checked_sub(delivery_range.start).map_err(time_error)?,
                    end.checked_sub(start).map_err(time_error)?,
                )
                .map_err(time_error)?,
                metadata: shot.metadata.clone(),
            });
        }
        let covers_start = projected.first().is_some_and(|shot| shot.range.start.is_zero());
        let covers_end = projected.last().and_then(|shot| shot.range.end().ok())
            == Some(delivery_range.duration);
        if !covers_start || !covers_end {
            return Err(dynamic_hdr_error(
                "Dynamic HDR Program does not cover the complete delivery range",
            ));
        }
        Ok(PreparedDynamicHdrProgram {
            source_program_id: program.id,
            standard: program.standard.clone(),
            provenance: program.provenance.clone(),
            delivery_range,
            shots: projected,
        })
    }

    /// Fork Program/Shot identities for an independent Sequence duplicate.
    pub fn fork_author_identities(&mut self) {
        let program_ids = self
            .programs
            .iter()
            .map(|program| (program.id, DynamicHdrProgramId::new()))
            .collect::<HashMap<_, _>>();
        for program in &mut self.programs {
            program.id = program_ids[&program.id];
            for shot in &mut program.shots {
                shot.id = DynamicHdrShotId::new();
            }
        }
        if let DynamicHdrDeliveryIntent::Remake { program_id } = &mut self.delivery_intent {
            *program_id = program_ids[program_id];
        }
    }
}

impl AuthoringFootprint for DynamicHdrAuthorState {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.delivery_intent)?;
        collector.collect(&self.programs)
    }
}

/// Frozen exact projection consumed by an Export Adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDynamicHdrProgram {
    /// Source author Program identity.
    pub source_program_id: DynamicHdrProgramId,
    /// Exact format row retained from the analyzed Program.
    pub standard: DynamicHdrStandard,
    /// Immutable analysis provenance retained for the Adapter.
    pub provenance: DynamicHdrAnalysisProvenance,
    /// Original Sequence-local range selected for delivery.
    pub delivery_range: TimelineTimeRange,
    /// Exact delivery-local projected shots.
    pub shots: Vec<PreparedDynamicHdrShot>,
}

/// One clipped delivery-local shot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDynamicHdrShot {
    /// Source author Shot identity.
    pub source_shot_id: DynamicHdrShotId,
    /// Human-readable source Shot label.
    pub name: String,
    /// Half-open range rebased to delivery-local zero.
    pub range: TimelineTimeRange,
    /// Format-specific metadata retained without reinterpretation.
    pub metadata: DynamicHdrShotMetadata,
}

fn validate_frame_grid_boundary(time: TimelineTime, frame_rate: Rational) -> Result<()> {
    let frame = time.to_frame_position(frame_rate, FrameRounding::Floor).map_err(time_error)?;
    let round_trip = TimelineTime::from_frame_position(FramePosition::new(
        frame.frame,
        Rational::new(frame_rate.den, frame_rate.num),
    ))
    .map_err(time_error)?;
    if round_trip != time {
        return Err(dynamic_hdr_error(format!(
            "Dynamic HDR boundary {time} is not aligned to the {frame_rate} fps presentation grid"
        )));
    }
    Ok(())
}

fn time_error(error: mondrian_core::TimelineTimeError) -> MondrianError {
    dynamic_hdr_error(error.to_string())
}

fn dynamic_hdr_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "dynamic_hdr_authoring".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        DynamicHdrShotMetadata, St2094Application4ShotMetadata, St2094DistributionPoint,
    };

    fn shot(name: &str, start: i64, duration: i64) -> DynamicHdrShot {
        DynamicHdrShot::new(
            name,
            TimelineTimeRange::new(
                TimelineTime::new(start, 25).expect("start"),
                TimelineTime::new(duration, 25).expect("duration"),
            )
            .expect("range"),
            DynamicHdrShotMetadata::St2094_40Application4(St2094Application4ShotMetadata {
                targeted_system_display_maximum_luminance: 1000,
                max_scl: [10_000, 9_000, 8_000],
                average_max_rgb: 1_500,
                distribution: vec![
                    St2094DistributionPoint { percentile: 50, value: 1_000 },
                    St2094DistributionPoint { percentile: 99, value: 9_500 },
                ],
                fraction_bright_pixels: 50,
                tone_mapping: None,
                color_saturation_weight: None,
            }),
        )
    }

    fn program() -> DynamicHdrProgram {
        DynamicHdrProgram::new(
            "ST 2094 program",
            DynamicHdrStandard::St2094_40Application4 { application_version: 0 },
            DynamicHdrAnalysisProvenance {
                adapter_id: "fixture-analyzer".to_owned(),
                adapter_version: "1.0".to_owned(),
                metadata_schema: "st2094-40:2020".to_owned(),
                visual_author_fingerprint: [3; 32],
                canonical_metadata_sha256: [4; 32],
            },
            [shot("A", 0, 25), shot("B", 25, 25)],
        )
    }

    #[test]
    fn author_state_rejects_gaps_and_mismatched_payloads() {
        let mut gap = program();
        gap.shots[1].range.start = TimelineTime::new(26, 25).expect("gap");
        let mut state = DynamicHdrAuthorState::default();
        assert!(state
            .apply(
                DynamicHdrAuthorEdit::InstallAnalyzedProgram { program: gap },
                Rational::FPS_25,
            )
            .is_err());

        let mut mismatch = program();
        mismatch.standard = DynamicHdrStandard::DolbyVision {
            cm_version: "4.0.2".to_owned(),
            bitstream_profile: 8,
            compatibility_id: Some(1),
            bitstream_level: Some(6),
        };
        assert!(state
            .apply(
                DynamicHdrAuthorEdit::InstallAnalyzedProgram { program: mismatch },
                Rational::FPS_25,
            )
            .is_err());
    }

    #[test]
    fn delivery_projection_clips_and_rebases_exact_ranges() {
        let program = program();
        let id = program.id;
        let mut state = DynamicHdrAuthorState::default();
        state
            .apply(
                DynamicHdrAuthorEdit::InstallAnalyzedProgram { program },
                Rational::FPS_25,
            )
            .expect("install");
        state
            .apply(
                DynamicHdrAuthorEdit::SetDeliveryIntent {
                    intent: DynamicHdrDeliveryIntent::Remake { program_id: id },
                },
                Rational::FPS_25,
            )
            .expect("intent");

        let range = TimelineTimeRange::new(
            TimelineTime::new(10, 25).expect("start"),
            TimelineTime::new(30, 25).expect("duration"),
        )
        .expect("range");
        let projected = state.project_delivery_range(id, range, [3; 32]).expect("projection");
        assert_eq!(projected.shots.len(), 2);
        assert_eq!(projected.shots[0].range.start, TimelineTime::ZERO);
        assert_eq!(projected.shots[1].range.end().expect("end"), range.duration);
        assert!(state.project_delivery_range(id, range, [9; 32]).is_err());
    }

    #[test]
    fn duplicate_forks_program_and_shot_identities_but_keeps_analysis() {
        let program = program();
        let old_program = program.id;
        let old_shots = program.shots.iter().map(|shot| shot.id).collect::<Vec<_>>();
        let mut state = DynamicHdrAuthorState::default();
        state
            .apply(
                DynamicHdrAuthorEdit::InstallAnalyzedProgram { program },
                Rational::FPS_25,
            )
            .expect("install");
        state
            .apply(
                DynamicHdrAuthorEdit::SetDeliveryIntent {
                    intent: DynamicHdrDeliveryIntent::Remake { program_id: old_program },
                },
                Rational::FPS_25,
            )
            .expect("intent");
        let fingerprint = state.programs()[0].provenance.visual_author_fingerprint;

        state.fork_author_identities();

        assert_ne!(state.programs()[0].id, old_program);
        assert!(state.programs()[0]
            .shots
            .iter()
            .zip(old_shots)
            .all(|(shot, old)| shot.id != old));
        assert_eq!(
            state.programs()[0].provenance.visual_author_fingerprint,
            fingerprint
        );
        assert_eq!(
            state.delivery_intent(),
            &DynamicHdrDeliveryIntent::Remake { program_id: state.programs()[0].id }
        );
    }
}
