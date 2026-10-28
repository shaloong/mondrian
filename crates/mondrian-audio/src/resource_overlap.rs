//! Frozen policy admission for prepared parameter-plan overlap.
//!
//! This is resource authority only. Native source/author acknowledgement,
//! installation claims, native-object Bind and graph equivalence remain their
//! existing independent gates. Logical plan weights are not allocator telemetry.

use crate::{
    AudioParameterEvent, AudioProgramRuntime, AudioRenderSession, AudioRuntimeBuildError,
    AudioRuntimeResourceCategory, AudioRuntimeResourceFootprint, AudioSessionCursorIdentity,
    NativeHostAllocationBorrow, NativeHostAllocationLedger, NativeHostAllocationOwner,
    PreparedAudioPlan,
};
use std::alloc::Layout;
use std::ops::Range;

/// Actual old Session and next plan/event-buffer owners for one local replacement.
///
/// Construction observes complete Vec capacities. The target Session may be a
/// nested materialization; admission resolves its exact private identity in the
/// current root. These observations neither install a plan nor authorize DSP.
pub struct AudioParameterPlanResourceReplacement<'a> {
    current: AudioSessionCursorIdentity,
    next_plan: &'a PreparedAudioPlan,
    events: &'a Vec<AudioParameterEvent>,
    lanes: &'a Vec<Range<usize>>,
}

impl<'a> AudioParameterPlanResourceReplacement<'a> {
    /// Observe the actual current Session and complete new reusable event Vecs.
    /// These owners must remain unchanged until the admission is consumed.
    pub fn new(
        current: &AudioRenderSession,
        next_plan: &'a PreparedAudioPlan,
        events: &'a Vec<AudioParameterEvent>,
        lanes: &'a Vec<Range<usize>>,
    ) -> Self {
        Self {
            current: current.native_owner_identity(),
            next_plan,
            events,
            lanes,
        }
    }
}

struct AdmittedPlanReplacement {
    ordinal: usize,
    current: AudioSessionCursorIdentity,
    next: mondrian_core::AudioPreparedPlanId,
    events_address: usize,
    events_layout: Layout,
    lanes_address: usize,
    lanes_layout: Layout,
}

/// Non-Clone resource receipt tied to the actual old root and local Sessions.
///
/// This receipt cannot mint native editing or installation authority. The known
/// extra-owner union includes exact allocator requests but is not a declaration
/// that unreported SDK, standard-library or foreign heap owners are free.
pub struct AudioParameterPlanResourceAdmission {
    root: AudioSessionCursorIdentity,
    replacements: Vec<AdmittedPlanReplacement>,
    footprint: AudioRuntimeResourceFootprint,
    peak_fixed_bytes: usize,
    peak_prepared_logical_bytes: usize,
    removed_logical_bytes: usize,
    inserted_logical_bytes: usize,
}

impl AudioParameterPlanResourceAdmission {
    /// Peak within the original fixed-residency policy ceiling, including new
    /// event Vecs, caller-reported complete extra owners and receipt bookkeeping.
    pub const fn peak_fixed_bytes(&self) -> usize {
        self.peak_fixed_bytes
    }

    /// Checked Root logical peak including this batch and existing held roles.
    pub const fn peak_prepared_logical_bytes(&self) -> usize {
        self.peak_prepared_logical_bytes
    }

    /// Revalidate exact root/local identities before the existing final swap.
    /// This performs no allocation, native call, cursor change or publication.
    pub fn validate_current(
        &self,
        runtime: &AudioProgramRuntime,
    ) -> Result<(), AudioRuntimeBuildError> {
        if runtime.resource_root_identity() != self.root {
            return Err(AudioRuntimeBuildError::StaleResourceAdmission);
        }
        for record in &self.replacements {
            if runtime.find_resource_session(record.current).is_none() {
                return Err(AudioRuntimeBuildError::StaleResourceAdmission);
            }
        }
        Ok(())
    }

    /// Consume the receipt while rechecking the exact prepared next-plan IDs.
    ///
    /// The returned commit is still not an installation permission. Existing
    /// author/Source CAS and all native/resource gates precede its publication.
    pub fn into_commit(
        self,
        runtime: &AudioProgramRuntime,
        replacements: &[AudioParameterPlanResourceReplacement<'_>],
    ) -> Result<AudioParameterPlanResourceCommit, AudioRuntimeBuildError> {
        self.validate_current(runtime)?;
        if self.replacements.len() != replacements.len() {
            return Err(AudioRuntimeBuildError::StaleResourceAdmission);
        }
        // The batch retains its original ordinal while sorting private records
        // to detect repeated Sessions. Every admitted owner is checked exactly
        // once; a repeated caller entry cannot stand in for another local owner.
        for record in &self.replacements {
            let replacement = replacements
                .get(record.ordinal)
                .ok_or(AudioRuntimeBuildError::StaleResourceAdmission)?;
            let events = replacement.events.host_allocation()?;
            let lanes = replacement.lanes.host_allocation()?;
            if record.current != replacement.current
                || record.next != replacement.next_plan.preparation_id()
                || record.events_address != events.address()
                || record.events_layout != events.layout()
                || record.lanes_address != lanes.address()
                || record.lanes_layout != lanes.layout()
            {
                return Err(AudioRuntimeBuildError::StaleResourceAdmission);
            }
        }
        // This read-only query is not a continuing reservation: other valid
        // preparations may have enrolled fees since the original observation.
        runtime.check_resource_ceiling(
            AudioRuntimeResourceCategory::PreparedLogicalBytes,
            sum(
                runtime.resource_footprint().prepared_logical_bytes,
                self.inserted_logical_bytes,
            )?,
        )?;
        Ok(AudioParameterPlanResourceCommit {
            root: self.root,
            footprint: self.footprint,
            removed_logical_bytes: self.removed_logical_bytes,
            inserted_logical_bytes: self.inserted_logical_bytes,
        })
    }
}

/// Consumed resource result for the same exclusive Runtime's bounded swap.
///
/// It has no public constructor or Clone. Only already admitted resource facts
/// are published; it never creates a second author or installation authority.
pub struct AudioParameterPlanResourceCommit {
    root: AudioSessionCursorIdentity,
    footprint: AudioRuntimeResourceFootprint,
    removed_logical_bytes: usize,
    inserted_logical_bytes: usize,
}

impl AudioParameterPlanResourceCommit {
    pub(crate) fn logical_role_weights(&self) -> (usize, usize) {
        (self.removed_logical_bytes, self.inserted_logical_bytes)
    }

    /// Read-only post-swap resource facts, not a new policy ceiling.
    pub const fn footprint(&self) -> AudioRuntimeResourceFootprint {
        self.footprint
    }

    /// The exact old root identity that must still own the final swap.
    pub const fn current_root(&self) -> AudioSessionCursorIdentity {
        self.root
    }
}

/// Admit all local prepared-plan replacements against the original root grant.
pub(crate) fn admit(
    runtime: &AudioProgramRuntime,
    replacements: &[AudioParameterPlanResourceReplacement<'_>],
    extra_owned_allocations: &Vec<NativeHostAllocationBorrow<'_>>,
) -> Result<AudioParameterPlanResourceAdmission, AudioRuntimeBuildError> {
    if replacements.is_empty() {
        return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
    }
    let grant = runtime.resource_grant();
    let original = runtime.resource_footprint();
    let root = runtime.resource_root_identity();
    let mut inserted_logical = 0usize;
    let mut removed_logical = 0usize;
    let mut inserted_parameter_bytes = 0usize;
    let mut removed_parameter_bytes = 0usize;
    let mut inserted_render_bytes = 0usize;
    let mut removed_render_bytes = 0usize;
    for replacement in replacements {
        let current = runtime
            .find_resource_session(replacement.current)
            .ok_or(AudioRuntimeBuildError::StaleResourceAdmission)?;
        let old_plan = current.resource_plan();
        let next_plan = replacement.next_plan;
        let mut old_shape = old_plan.schedule_summary();
        let mut next_shape = next_plan.schedule_summary();
        // Parameter curves and their sample-event extent may change. Any
        // signal topology, DSP, PDC or retained Session shape change belongs to
        // the full candidate path, not an ordinary parameter-plan handoff.
        old_shape.automation_curve_count = 0;
        next_shape.automation_curve_count = 0;
        old_shape.automation_event_span_count = 0;
        next_shape.automation_event_span_count = 0;
        old_shape.maximum_parameter_events_per_block = 0;
        next_shape.maximum_parameter_events_per_block = 0;
        if old_plan.contract() != next_plan.contract()
            || old_plan.kernel_backend() != next_plan.kernel_backend()
            || old_shape != next_shape
            || replacement.current.prepared_plan_id == next_plan.preparation_id()
            || replacement.events.capacity()
                < next_plan.schedule_summary().maximum_parameter_events_per_block
            || replacement.lanes.capacity()
                < next_plan
                    .schedule
                    .processors
                    .iter()
                    .map(|processor| processor.parameter_ids.len())
                    .max()
                    .unwrap_or(0)
        {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
        }
        let old = current.current_resource_footprint()?;
        let next = next_plan.session_resource_footprint()?;
        inserted_logical = sum(inserted_logical, next.prepared_logical_bytes)?;
        removed_logical = sum(removed_logical, old.prepared_logical_bytes)?;
        removed_parameter_bytes = sum(removed_parameter_bytes, old.parameter_event_bytes)?;
        // Output contract/descriptor Vec and String clones can own different
        // capacities while preserving the exact same validated graph shape.
        removed_render_bytes = sum(removed_render_bytes, old.render_scratch_bytes)?;
        inserted_render_bytes = sum(inserted_render_bytes, next.render_scratch_bytes)?;
        let event_layout = replacement.events.host_allocation()?.layout();
        let lane_layout = replacement.lanes.host_allocation()?.layout();
        inserted_parameter_bytes = sum(
            inserted_parameter_bytes,
            sum(event_layout.size(), lane_layout.size())?,
        )?;
    }
    let peak_logical = runtime.check_resource_ceiling(
        AudioRuntimeResourceCategory::PreparedLogicalBytes,
        sum(original.prepared_logical_bytes, inserted_logical)?,
    )?;

    // Bookkeeping is bounded and charged before its first allocation. The
    // caller's allocation-receipt Vec itself is another complete extra owner.
    let requested_records =
        Layout::array::<AdmittedPlanReplacement>(replacements.len()).map_err(|_| {
            AudioRuntimeBuildError::ResourceFootprint(
                crate::AudioResourceFootprintError::ExtentOverflow,
            )
        })?;
    let owner_limit = replacements
        .len()
        .checked_mul(2)
        .and_then(|count| count.checked_add(extra_owned_allocations.len()))
        .and_then(|count| count.checked_add(1))
        .ok_or(AudioRuntimeBuildError::ResourceFootprint(
            crate::AudioResourceFootprintError::ExtentOverflow,
        ))?;
    let requested_ledger_bytes =
        NativeHostAllocationLedger::storage_layout(owner_limit)?.bytes()?;
    let lower_peak = sum(
        original.fixed_resident_bytes,
        sum(requested_records.size(), requested_ledger_bytes)?,
    )?;
    runtime.check_resource_ceiling(AudioRuntimeResourceCategory::FixedResidentBytes, lower_peak)?;
    let mut records = Vec::new();
    records
        .try_reserve_exact(replacements.len())
        .map_err(|_| AudioRuntimeBuildError::ResourceAdmissionAllocation)?;
    let actual_record_bytes = Layout::array::<AdmittedPlanReplacement>(records.capacity())
        .map_err(|_| {
            AudioRuntimeBuildError::ResourceFootprint(
                crate::AudioResourceFootprintError::ExtentOverflow,
            )
        })?
        .size();
    let fixed_before_ledger = sum(original.fixed_resident_bytes, actual_record_bytes)?;
    runtime.check_resource_ceiling(
        AudioRuntimeResourceCategory::FixedResidentBytes,
        fixed_before_ledger,
    )?;
    let remaining = grant
        .max_fixed_resident_bytes
        .checked_sub(fixed_before_ledger)
        .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
    let mut ledger = NativeHostAllocationLedger::new(owner_limit, remaining)?;
    ledger.record(extra_owned_allocations.host_allocation()?)?;
    for (ordinal, replacement) in replacements.iter().enumerate() {
        ledger.record(replacement.events.host_allocation()?)?;
        ledger.record(replacement.lanes.host_allocation()?)?;
        let events = replacement.events.host_allocation()?;
        let lanes = replacement.lanes.host_allocation()?;
        records.push(AdmittedPlanReplacement {
            ordinal,
            current: replacement.current,
            next: replacement.next_plan.preparation_id(),
            events_address: events.address(),
            events_layout: events.layout(),
            lanes_address: lanes.address(),
            lanes_layout: lanes.layout(),
        });
    }
    for &allocation in extra_owned_allocations {
        ledger.record(allocation)?;
    }
    // Batch sorting avoids quadratic duplicate detection. Final validation
    // resolves the caller's retained order by exact local Session identity.
    records.sort_unstable_by_key(|record| record.current.session_id);
    if records
        .windows(2)
        .any(|pair| pair[0].current.session_id == pair[1].current.session_id)
    {
        return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
    }
    let observed = ledger.finish()?;
    let peak_fixed = sum(fixed_before_ledger, observed.allocation_bytes())?;
    runtime.check_resource_ceiling(AudioRuntimeResourceCategory::FixedResidentBytes, peak_fixed)?;
    let mut footprint = original;
    footprint.prepared_logical_bytes = replace(
        original.prepared_logical_bytes,
        removed_logical,
        inserted_logical,
    )?;
    footprint.parameter_event_bytes = replace(
        original.parameter_event_bytes,
        removed_parameter_bytes,
        inserted_parameter_bytes,
    )?;
    footprint.render_scratch_bytes = replace(
        original.render_scratch_bytes,
        removed_render_bytes,
        inserted_render_bytes,
    )?;
    footprint.fixed_resident_bytes = replace(
        original.fixed_resident_bytes,
        sum(removed_parameter_bytes, removed_render_bytes)?,
        sum(inserted_parameter_bytes, inserted_render_bytes)?,
    )?;
    runtime.check_resource_ceiling(
        AudioRuntimeResourceCategory::FixedResidentBytes,
        footprint.fixed_resident_bytes,
    )?;
    runtime.check_resource_ceiling(
        AudioRuntimeResourceCategory::PreparedLogicalBytes,
        footprint.prepared_logical_bytes,
    )?;
    Ok(AudioParameterPlanResourceAdmission {
        root,
        replacements: records,
        footprint,
        peak_fixed_bytes: peak_fixed,
        peak_prepared_logical_bytes: peak_logical,
        removed_logical_bytes: removed_logical,
        inserted_logical_bytes: inserted_logical,
    })
}

fn sum(left: usize, right: usize) -> Result<usize, AudioRuntimeBuildError> {
    left.checked_add(right).ok_or(AudioRuntimeBuildError::ResourceFootprint(
        crate::AudioResourceFootprintError::ExtentOverflow,
    ))
}

fn replace(total: usize, old: usize, next: usize) -> Result<usize, AudioRuntimeBuildError> {
    let retained = total
        .checked_sub(old)
        .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
    sum(retained, next)
}

#[cfg(test)]
mod qualification {
    use super::*;
    use crate::{
        AudioCompileRequest, AudioMediaResolver, AudioProcessingMode, AudioRenderContract,
        AudioRenderRequest, NativeHostLedgerError, ResolvedAudioSource,
    };
    use mondrian_core::{AssetId, AudioChannelLayout, AudioSourceComponentId};
    use mondrian_timeline::Sequence;
    use std::sync::Arc;

    struct UnusedMedia;
    impl AudioMediaResolver for UnusedMedia {
        fn resolve(
            &self,
            _: AssetId,
            _: AudioSourceComponentId,
            _: u32,
        ) -> Result<ResolvedAudioSource, String> {
            Err("empty authored signal has no media dependency".to_owned())
        }
    }

    fn contract() -> AudioRenderContract {
        AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 64,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 4 * 1024 * 1024,
            public_output_lookahead_budget_frames: 48_000,
            compensation_delay_scratch_budget_bytes: 4 * 1024 * 1024,
        }
    }

    fn sequence() -> Sequence {
        let mut sequence = Sequence::new("frozen root resource admission");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
        sequence.settings.audio_sample_rate = 48_000;
        sequence
    }

    fn build(sequence: &Sequence, grant: crate::AudioRuntimeResourceGrant) -> AudioProgramRuntime {
        AudioProgramRuntime::build_with_resource_grant(
            sequence,
            &[],
            &UnusedMedia,
            contract(),
            None,
            grant,
        )
        .expect("real prepared exclusive Runtime")
    }

    fn next_plan(sequence: &Sequence) -> PreparedAudioPlan {
        let program = crate::compile_audio_program(
            sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
        )
        .expect("sole compiler");
        PreparedAudioPlan::prepare(Arc::new(program), contract()).expect("sole preparer")
    }

    fn local(runtime: &AudioProgramRuntime) -> &AudioRenderSession {
        runtime
            .find_resource_session(runtime.resource_root_identity())
            .expect("actual local Session")
    }

    #[test]
    fn parameter_plan_overlap_uses_original_grant_and_keeps_current_pcm() {
        let sequence = sequence();
        let grant = crate::AudioRuntimeResourceGrant::new(1, 4 * 1024 * 1024, 4 * 1024 * 1024);
        let mut runtime = build(&sequence, grant);
        let initial = runtime.resource_footprint();
        assert_eq!(runtime.resource_grant(), grant);
        let next = next_plan(&sequence);
        let events = Vec::with_capacity(11);
        let lanes = Vec::with_capacity(7);
        let replacements = [AudioParameterPlanResourceReplacement::new(
            local(&runtime),
            &next,
            &events,
            &lanes,
        )];
        let extra = Vec::new();
        let admission = runtime
            .admit_parameter_plan_overlap(&replacements, &extra)
            .expect("original ceiling admits peak");
        assert!(admission.peak_fixed_bytes() > initial.fixed_resident_bytes);
        assert!(admission.peak_prepared_logical_bytes() > initial.prepared_logical_bytes);
        let commit = admission
            .into_commit(&runtime, &replacements)
            .expect("exact old/new owners still current");
        assert_eq!(commit.current_root(), runtime.resource_root_identity());
        assert_eq!(
            commit.footprint().runtime_occurrences,
            initial.runtime_occurrences
        );
        assert_eq!(runtime.resource_footprint(), initial);
        let mut pcm = [123.0; 8];
        runtime
            .render_into(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: 0,
                    frames: 8,
                },
                &mut pcm,
            )
            .expect("current graph is executable");
        assert_eq!(pcm, [0.0; 8]);
    }

    #[test]
    fn parameter_plan_overlap_refuses_new_plan_even_when_each_plan_fits_alone() {
        let sequence = sequence();
        let baseline = build(
            &sequence,
            crate::AudioRuntimeResourceGrant::new(1, usize::MAX, usize::MAX),
        );
        let original = baseline.resource_footprint();
        let runtime = build(
            &sequence,
            crate::AudioRuntimeResourceGrant::new(
                1,
                4 * 1024 * 1024,
                original.prepared_logical_bytes,
            ),
        );
        let next = next_plan(&sequence);
        let events = Vec::new();
        let lanes = Vec::new();
        let replacements = [AudioParameterPlanResourceReplacement::new(
            local(&runtime),
            &next,
            &events,
            &lanes,
        )];
        let error = runtime
            .admit_parameter_plan_overlap(&replacements, &Vec::new())
            .err()
            .expect("old+new peak is refused");
        assert!(
            matches!(error, AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes, granted, required,
        } if granted == original.prepared_logical_bytes && required > granted)
        );
        assert_eq!(runtime.resource_footprint(), original);
    }

    #[test]
    fn parameter_plan_overlap_refuses_actual_reserved_capacity_without_mutating_old_root() {
        let sequence = sequence();
        let runtime = build(
            &sequence,
            crate::AudioRuntimeResourceGrant::new(1, 256 * 1024, 4 * 1024 * 1024),
        );
        let initial = runtime.resource_footprint();
        let next = next_plan(&sequence);
        let events = Vec::<AudioParameterEvent>::with_capacity(128 * 1024);
        assert!(events.is_empty());
        let lanes = Vec::new();
        let replacements = [AudioParameterPlanResourceReplacement::new(
            local(&runtime),
            &next,
            &events,
            &lanes,
        )];
        let error = runtime
            .admit_parameter_plan_overlap(&replacements, &Vec::new())
            .err()
            .expect("unfilled owned capacity is charged");
        assert!(matches!(
            error,
            AudioRuntimeBuildError::ResourceOwner(NativeHostLedgerError::GrantExceeded { .. })
        ));
        assert_eq!(runtime.resource_footprint(), initial);
    }

    #[test]
    fn parameter_plan_overlap_refuses_foreign_or_duplicate_session_targets() {
        let sequence = sequence();
        let grant = crate::AudioRuntimeResourceGrant::new(2, 4 * 1024 * 1024, 4 * 1024 * 1024);
        let runtime = build(&sequence, grant);
        let foreign = build(&sequence, grant);
        let next = next_plan(&sequence);
        let events = Vec::new();
        let lanes = Vec::new();
        let foreign_request = [AudioParameterPlanResourceReplacement::new(
            local(&foreign),
            &next,
            &events,
            &lanes,
        )];
        assert!(matches!(
            runtime.admit_parameter_plan_overlap(&foreign_request, &Vec::new()),
            Err(AudioRuntimeBuildError::StaleResourceAdmission)
        ));
        let duplicates = [
            AudioParameterPlanResourceReplacement::new(local(&runtime), &next, &events, &lanes),
            AudioParameterPlanResourceReplacement::new(local(&runtime), &next, &events, &lanes),
        ];
        assert!(matches!(
            runtime.admit_parameter_plan_overlap(&duplicates, &Vec::new()),
            Err(AudioRuntimeBuildError::InvalidResourceReplacement)
        ));
    }

    #[test]
    fn parameter_plan_overlap_extra_actual_owner_alias_is_charged_once() {
        let sequence = sequence();
        let runtime = build(
            &sequence,
            crate::AudioRuntimeResourceGrant::new(1, 4 * 1024 * 1024, 4 * 1024 * 1024),
        );
        let next = next_plan(&sequence);
        let events = Vec::new();
        let lanes = Vec::new();
        let replacements = [AudioParameterPlanResourceReplacement::new(
            local(&runtime),
            &next,
            &events,
            &lanes,
        )];
        let payload = Vec::<u8>::with_capacity(64 * 1024);
        let mut extra = Vec::with_capacity(2);
        extra.push(payload.host_allocation().expect("actual payload allocation"));
        let first = runtime
            .admit_parameter_plan_overlap(&replacements, &extra)
            .expect("one actual extra owner");
        extra.push(payload.host_allocation().expect("alias of same allocation"));
        let second = runtime
            .admit_parameter_plan_overlap(&replacements, &extra)
            .expect("same owner twice");
        // The caller Vec capacity is unchanged. Only one more fixed ledger slot
        // is needed; the same 64KiB physical payload is never charged again.
        let slot_growth = crate::NativeHostAllocationLedger::storage_layout(5)
            .expect("bounded layout")
            .bytes()
            .expect("size")
            - crate::NativeHostAllocationLedger::storage_layout(4)
                .expect("bounded layout")
                .bytes()
                .expect("size");
        assert_eq!(
            second.peak_fixed_bytes() - first.peak_fixed_bytes(),
            slot_growth
        );
    }

    #[test]
    fn parameter_plan_overlap_receipt_cannot_follow_another_root_or_next_plan() {
        let sequence = sequence();
        let grant = crate::AudioRuntimeResourceGrant::new(1, 4 * 1024 * 1024, 4 * 1024 * 1024);
        let runtime = build(&sequence, grant);
        let next = next_plan(&sequence);
        let other_next = next_plan(&sequence);
        let events = Vec::new();
        let lanes = Vec::new();
        let replacements = [AudioParameterPlanResourceReplacement::new(
            local(&runtime),
            &next,
            &events,
            &lanes,
        )];
        let admission = runtime
            .admit_parameter_plan_overlap(&replacements, &Vec::new())
            .expect("admitted");
        let foreign = build(&sequence, grant);
        assert!(matches!(
            admission.validate_current(&foreign),
            Err(AudioRuntimeBuildError::StaleResourceAdmission)
        ));
        let changed = [AudioParameterPlanResourceReplacement::new(
            local(&runtime),
            &other_next,
            &events,
            &lanes,
        )];
        assert!(matches!(
            admission.into_commit(&runtime, &changed),
            Err(AudioRuntimeBuildError::StaleResourceAdmission)
        ));
    }
}
