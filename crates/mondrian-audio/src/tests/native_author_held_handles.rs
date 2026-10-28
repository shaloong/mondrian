//! External Preserved handoff owner lifetime and frozen logical-policy controls.
//! Uses the existing public Runtime/Source seam and independent owned processor.

use super::*;

/// Keep the existing caller-owned retirement frontier explicit in this control.
/// This helper adds no Runtime ownership/registry and does not modify A's helper.
fn install_retaining_external_handle(
    fixture: &mut PreflightFixture,
    proposal: &mondrian_core::NativeAudioAuthorProposal,
    programs: &[NativeAudioAuthorProgram],
) -> crate::PreparedNativeAudioParameterHandoff {
    assert!(!proposal.terminal());
    assert!(proposal.state().is_none());
    assert!(!fixture.inner.runtime.native_author_state_entry_required(proposal).unwrap());
    let old_identity = fixture.inner.runtime.resource_root_identity();
    let old_footprint = fixture.inner.runtime.resource_footprint();
    let old_creates = fixture.registry.calls.creates.load(Ordering::Relaxed);
    let old_entries = fixture.inner.probe.lock().unwrap().entries;
    let old_object = fixture.inner.probe.lock().unwrap().binding.unwrap().expected_object;
    let (registration, destination) = fixture.inner.source.register().unwrap();
    let old_claim = fixture.inner.source.current_claim().unwrap().unwrap();
    let reservation =
        fixture.inner.source.reserve_install(&registration, Some(&old_claim)).unwrap();
    let mut authorization =
        fixture.inner.source.authorize_runtime_installation(reservation).unwrap();
    let mut prepared = fixture
        .inner
        .runtime
        .prepare_native_author_parameter_handoff(
            programs,
            proposal,
            destination,
            &ExecutionCancellationToken::new(),
        )
        .expect("first actual handoff fits the original frozen grant");
    assert_eq!(fixture.inner.runtime.resource_root_identity(), old_identity);
    assert_eq!(
        fixture.inner.runtime.resource_footprint(),
        old_footprint,
        "preparation does not publish the replacement steady footprint"
    );
    let receipt = fixture
        .inner
        .runtime
        .install_native_author_parameter_handoff(
            &mut prepared,
            &mut authorization,
            &ExecutionCancellationToken::new(),
        )
        .expect("first actual same-object Source installation");
    assert_eq!(
        receipt.continuity,
        NativeAudioParameterHandoffContinuity::Preserved
    );
    assert_eq!(
        receipt.target.local_session,
        fixture.inner.target.local_session
    );
    assert_eq!(
        receipt.target.continuity_epoch,
        fixture.inner.target.continuity_epoch
    );
    assert_ne!(receipt.target.binding, fixture.inner.target.binding);
    assert_eq!(
        fixture.registry.calls.creates.load(Ordering::Relaxed),
        old_creates
    );
    {
        let probe = fixture.inner.probe.lock().unwrap();
        assert_eq!(probe.entries, old_entries);
        assert_eq!(probe.binding.unwrap().expected_object, old_object);
    }
    fixture.inner.target = receipt.target;
    // Author/Source final guards have ended. The caller deliberately owns the
    // old Plan/scratch and separately decides when to Ack and release them.
    prepared
}

#[test]
fn native_author_held_installed_handle_is_charged_before_next_prepare() {
    let spec = preflight_spec(PreflightPlacement::Output);
    // Observe actual L0/L1/L2 with the accepted public prepare/install/Ack path.
    // No private footprint is changed and no guessed allocator bytes are used.
    let (l0, l1, l2) = {
        let mut control = preflight_build(&spec, usize::MAX);
        let l0 = control.inner.runtime.resource_footprint().prepared_logical_bytes;
        let first = control.inner.value(0.75);
        let first_programs = preflight_programs(&mut control, 0.75);
        preflight_install(&mut control, &first, &first_programs);
        let l1 = control.inner.runtime.resource_footprint().prepared_logical_bytes;
        let second = control.inner.value(0.25);
        let second_programs = preflight_programs(&mut control, 0.25);
        preflight_install(&mut control, &second, &second_programs);
        let l2 = control.inner.runtime.resource_footprint().prepared_logical_bytes;
        assert_eq!(
            control.inner.runtime.resource_footprint().runtime_occurrences,
            1
        );
        (l0, l1, l2)
    }; // All control Runtime/Plan and temporary Program owners end here.
    assert!(l0 > 0);
    assert_eq!(
        l1, l2,
        "two scalar updates keep the actual prepared logical shape"
    );
    let grant = l1.checked_mul(2).unwrap();
    let first_peak = l0.checked_add(l1).unwrap();
    let per_call_next_peak = l1.checked_add(l2).unwrap();
    let all_live_peak = l0.checked_add(l1).and_then(|sum| sum.checked_add(l2)).unwrap();
    assert!(
        first_peak <= grant,
        "the first real preparation genuinely fits"
    );
    assert_eq!(per_call_next_peak, grant);
    assert!(all_live_peak > grant);

    let mut fixture = preflight_build(&spec, grant);
    let original_grant = fixture.inner.runtime.resource_grant();
    assert_eq!(original_grant.max_prepared_logical_bytes, grant);
    assert_eq!(
        fixture.inner.runtime.resource_footprint().prepared_logical_bytes,
        l0
    );
    let old_identity = fixture.inner.runtime.resource_root_identity();
    let old_plan_weak = {
        let old_session = fixture.inner.runtime.find_resource_session(old_identity).unwrap();
        // Borrow the actual Arc only; this control retains no extra strong owner.
        Arc::downgrade(old_session.shared_resource_plan())
    };
    let first = fixture.inner.value(0.75);
    let first_programs = preflight_programs(&mut fixture, 0.75);
    let prepared_first = install_retaining_external_handle(&mut fixture, &first, &first_programs);
    let installed = fixture.inner.runtime.resource_root_identity();
    assert_ne!(installed.prepared_plan_id, old_identity.prepared_plan_id);
    assert_eq!(installed.session_id, old_identity.session_id);
    assert_eq!(
        fixture.inner.runtime.resource_footprint().prepared_logical_bytes,
        l1
    );
    let held_strong_owners = old_plan_weak.strong_count();
    assert!(
        held_strong_owners > 0,
        "the external handle actually retains the old Plan"
    );
    {
        let actual_old = old_plan_weak.upgrade().expect("actual old retired owner survives");
        assert_eq!(actual_old.preparation_id(), old_identity.prepared_plan_id);
    } // Release the temporary upgrade BEFORE Ack/next prepare.
    assert_eq!(old_plan_weak.strong_count(), held_strong_owners);
    fixture.inner.ack(&first);
    assert!(
        old_plan_weak.strong_count() > 0,
        "Ack is not external handle retirement"
    );
    let mut pcm = [93.0];
    fixture
        .inner
        .runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 1,
                frames: 1,
            },
            &mut pcm,
        )
        .expect("actual installed current Plan remains usable after exact Ack");
    assert_eq!(pcm, [1.5]);

    let second = fixture.inner.value(0.25);
    let second_programs = preflight_programs(&mut fixture, 0.25);
    assert!(!second.terminal());
    assert!(second.state().is_none());
    assert!(!fixture.inner.runtime.native_author_state_entry_required(&second).unwrap());
    let (_, destination) = fixture.inner.source.register().unwrap();
    let claim_before = fixture.inner.source.current_claim().unwrap().unwrap();
    let before = fixture.inner.runtime.resource_footprint();
    let result = fixture.inner.runtime.prepare_native_author_parameter_handoff(
        &second_programs,
        &second,
        destination,
        &ExecutionCancellationToken::new(),
    );
    eprintln!(
        "HELD_HANDOFF_NEXT_PREPARE: L0={l0}; L1={l1}; L2={l2}; frozen={grant}; per_call={per_call_next_peak}; all_live={all_live_peak}; old_plan_alive={}; accepted={}; error={:?}",
        old_plan_weak.strong_count() > 0, result.is_ok(), result.as_ref().err(),
    );
    assert!(
        old_plan_weak.strong_count() > 0,
        "a next-call refusal cannot release the earlier caller-owned retired Plan"
    );
    assert_eq!(fixture.inner.runtime.resource_root_identity(), installed);
    assert_eq!(fixture.inner.runtime.resource_footprint(), before);
    assert_eq!(fixture.inner.runtime.resource_grant(), original_grant);
    assert_eq!(
        fixture.inner.source.current_claim().unwrap().unwrap().generation(),
        claim_before.generation()
    );
    // If this run returns Ok, discard its new external preparation outside any
    // final guard before reporting the desired resource-refusal failure.
    let refusal = result.err();
    let correctly_refused = matches!(refusal.as_ref(), Some(
        NativeAudioParameterHandoffError::Resources(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
            required, granted,
        })
    ) if *required == all_live_peak && *granted == grant);
    drop(prepared_first);
    assert!(
        old_plan_weak.upgrade().is_none(),
        "dropping the last external handle retires the real old Plan immediately"
    );
    if correctly_refused {
        // The same original grant now covers exactly current + one replacement.
        let (_, retry_destination) = fixture.inner.source.register().unwrap();
        let retry = fixture
            .inner
            .runtime
            .prepare_native_author_parameter_handoff(
                &second_programs,
                &second,
                retry_destination,
                &ExecutionCancellationToken::new(),
            )
            .expect("external retirement releases its charge without enlarging the grant");
        drop(retry);
    }
    assert!(correctly_refused,
        "expected exact original-grant logical refusal for all live Plans: required={all_live_peak}, granted={grant}; actual={refusal:?}");
}
