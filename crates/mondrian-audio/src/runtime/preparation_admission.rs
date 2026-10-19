//! Prospective prepared-policy charges in the existing closure admission ledger.
//!
//! This is not an execution permit or a physical allocator/RSS bound. The
//! private receipt is consumed once by the synchronous builder. No second
//! grant, reservation map, Note ledger or native identity is introduced.

use super::{
    AudioRuntimeAdmissionLedger, AudioRuntimeBuildError, AudioRuntimeResourceCategory,
    AudioRuntimeResourceFootprint, AudioSessionResourceFootprint, ProgramOutputId, SequenceId,
};

/// Non-cloneable accounting receipt for one existing ledger's local Plan charge.
pub(super) struct PreparedPlanReservation {
    sequence_id: SequenceId,
    output_id: ProgramOutputId,
    requested_bytes: usize,
}

impl AudioRuntimeAdmissionLedger {
    /// Consume the existing direct-root accounting receipt through actual Plan
    /// preparation, then complete the sole closure ledger before Session create.
    /// The caller checks root/window/precompiled scope before dependency IO.
    pub(super) fn prepare_admitted_root_notes(
        &mut self,
        reservation: PreparedPlanReservation,
        program: std::sync::Arc<crate::CompiledAudioProgram>,
        contract: crate::AudioRenderContract,
        dependencies: &crate::schedule::AudioPreparationDependencies,
        processor_resolver: &dyn crate::AudioProcessorResolver,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<std::sync::Arc<crate::PreparedAudioPlan>, AudioRuntimeBuildError> {
        if reservation.output_id != program.output_id() {
            return Err(AudioRuntimeBuildError::StaleResourceAdmission);
        }
        let plan = std::sync::Arc::new(crate::PreparedAudioPlan::prepare_admitted_root_notes(
            program,
            contract,
            dependencies,
            processor_resolver,
            native_child_settlement,
        )?);
        self.complete_prepared_session(reservation, plan.session_resource_footprint()?)?;
        Ok(plan)
    }

    /// Admit only the direct unwindowed compiler's nonempty selected Note family.
    /// B0 and all four lowering phases use their existing consumers; Ac is the
    /// compiler's checked request. The receipt still belongs to this ledger.
    pub(super) fn reserve_compiled_note_plan(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        graph: crate::plan::CompiledAudioGraph<'_>,
        notes: crate::plan::CompiledNoteStorageRequest,
    ) -> Result<PreparedPlanReservation, AudioRuntimeBuildError> {
        let base = crate::PreparedAudioPlan::requested_base_logical_bytes_for_graph(graph)?;
        let peak = crate::schedule::requested_note_lowering_peak_bytes(
            notes.lanes,
            notes.events,
            notes.key_bytes,
        )?;
        let requested = base
            .checked_add(notes.compiler_bytes)
            .and_then(|bytes| bytes.checked_add(peak))
            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
                sequence_id,
                output_id,
            })?;
        self.reserve_prepared_plan(sequence_id, output_id, requested)
    }

    pub(super) fn reserve_prepared_plan(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        requested_bytes: usize,
    ) -> Result<PreparedPlanReservation, AudioRuntimeBuildError> {
        self.footprint.prepared_logical_bytes.checked_add(requested_bytes).ok_or(
            AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
                sequence_id,
                output_id,
            },
        )?;
        self.reserve(
            sequence_id,
            output_id,
            AudioRuntimeResourceFootprint {
                prepared_logical_bytes: requested_bytes,
                ..AudioRuntimeResourceFootprint::default()
            },
        )?;
        Ok(PreparedPlanReservation { sequence_id, output_id, requested_bytes })
    }

    pub(super) fn complete_prepared_session(
        &mut self,
        reservation: PreparedPlanReservation,
        session: AudioSessionResourceFootprint,
    ) -> Result<(), AudioRuntimeBuildError> {
        if session.prepared_logical_bytes > reservation.requested_bytes {
            return Err(AudioRuntimeBuildError::Compile(
                crate::AudioCompileError::InvalidPreparedGraph(
                    "actual prepared logical policy exceeds its prospective reservation".to_owned(),
                ),
            ));
        }
        // Stage replacement of this local charge, keeping all pending ancestors
        // and completed siblings. The existing reserve() checks every unchanged
        // ceiling before this completion publishes any accounting mutation.
        let mut completed = *self;
        completed.footprint.prepared_logical_bytes = completed
            .footprint
            .prepared_logical_bytes
            .checked_sub(reservation.requested_bytes)
            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
                sequence_id: reservation.sequence_id,
                output_id: reservation.output_id,
            })?;
        completed.reserve_session(reservation.sequence_id, reservation.output_id, session)?;
        self.footprint = completed.footprint;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AudioRuntimeResourceGrant;

    fn session(prepared: usize, fixed: usize) -> AudioSessionResourceFootprint {
        AudioSessionResourceFootprint {
            prepared_logical_bytes: prepared,
            fixed_resident_bytes: fixed,
            ..AudioSessionResourceFootprint::default()
        }
    }

    #[test]
    fn ancestor_and_completed_siblings_remain_charged_at_each_descendant() {
        let sequence = SequenceId::new();
        let output = ProgramOutputId::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::new(
            3,
            15 + 3 * std::mem::size_of::<super::super::native_reprepare::NativeReprepareState>(),
            100,
        ));
        let root = ledger.reserve_prepared_plan(sequence, output, 40).unwrap();
        let first = ledger.reserve_prepared_plan(sequence, output, 30).unwrap();
        ledger.complete_prepared_session(first, session(20, 5)).unwrap();
        assert_eq!(ledger.footprint.prepared_logical_bytes, 60);
        let second = ledger.reserve_prepared_plan(sequence, output, 40).unwrap();
        assert_eq!(ledger.footprint.prepared_logical_bytes, 100);
        let before = ledger.footprint;
        assert!(matches!(
            ledger.reserve_prepared_plan(sequence, output, 1),
            Err(AudioRuntimeBuildError::ResourceGrantExceeded {
                category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
                required: 101,
                granted: 100,
                ..
            })
        ));
        assert_eq!(ledger.footprint, before);
        ledger.complete_prepared_session(second, session(30, 5)).unwrap();
        ledger.complete_prepared_session(root, session(40, 5)).unwrap();
        assert_eq!(ledger.footprint.prepared_logical_bytes, 90);
        assert_eq!(
            ledger.footprint.fixed_resident_bytes,
            15 + 3 * std::mem::size_of::<super::super::native_reprepare::NativeReprepareState>()
        );
        assert_eq!(ledger.footprint.runtime_occurrences, 3);
    }

    #[test]
    fn completion_cannot_enlarge_projection_or_publish_a_failed_fixed_charge() {
        let sequence = SequenceId::new();
        let output = ProgramOutputId::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::new(
            1,
            5 + std::mem::size_of::<super::super::native_reprepare::NativeReprepareState>(),
            40,
        ));
        let reservation = ledger.reserve_prepared_plan(sequence, output, 40).unwrap();
        let before = ledger.footprint;
        assert!(ledger.complete_prepared_session(reservation, session(41, 0)).is_err());
        assert_eq!(ledger.footprint, before);

        // A fresh failed builder, rather than reusing the consumed receipt.
        let mut ledger = AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::new(
            1,
            5 + std::mem::size_of::<super::super::native_reprepare::NativeReprepareState>(),
            40,
        ));
        let reservation = ledger.reserve_prepared_plan(sequence, output, 40).unwrap();
        let before = ledger.footprint;
        assert!(matches!(
            ledger.complete_prepared_session(reservation, session(30, 6)),
            Err(AudioRuntimeBuildError::ResourceGrantExceeded {
                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                required,
                granted,
                ..
            }) if required == 6 + std::mem::size_of::<super::super::native_reprepare::NativeReprepareState>()
                && granted == 5 + std::mem::size_of::<super::super::native_reprepare::NativeReprepareState>()
        ));
        assert_eq!(ledger.footprint, before);
    }

    #[test]
    fn prospective_total_uses_checked_arithmetic_without_saturation() {
        let sequence = SequenceId::new();
        let output = ProgramOutputId::new();
        let mut ledger =
            AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::new(1, 0, usize::MAX));
        let _reservation = ledger.reserve_prepared_plan(sequence, output, usize::MAX).unwrap();
        let before = ledger.footprint;
        assert!(matches!(
            ledger.reserve_prepared_plan(sequence, output, 1),
            Err(AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
                ..
            })
        ));
        assert_eq!(ledger.footprint, before);
    }
}
