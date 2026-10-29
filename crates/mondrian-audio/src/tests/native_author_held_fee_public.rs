//! Conditional logical-policy tests over actual public Runtime/Source owners.
//! Test ABI owners qualify logical policy; DLL, physical and RT gates are separate.
use super::*;

fn fee_public_prepare(
    fixture: &mut PreflightFixture,
    proposal: &mondrian_core::NativeAudioAuthorProposal,
    programs: &[NativeAudioAuthorProgram],
) -> (
    crate::PreparedNativeAudioParameterHandoff,
    mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
) {
    assert!(!proposal.terminal());
    assert!(proposal.state().is_none());
    assert!(!fixture.inner.runtime.native_author_state_entry_required(proposal).unwrap());
    let root = fixture.inner.runtime.resource_root_identity();
    let footprint = fixture.inner.runtime.resource_footprint();
    let cursors = preflight_cursors(&fixture.inner.runtime);
    let claim = fixture.inner.source.current_claim().unwrap().unwrap();
    let (registration, destination) = fixture.inner.source.register().unwrap();
    let reservation = fixture.inner.source.reserve_install(&registration, Some(&claim)).unwrap();
    let authorization = fixture.inner.source.authorize_runtime_installation(reservation).unwrap();
    let result = fixture.inner.runtime.prepare_native_author_parameter_handoff(
        programs,
        proposal,
        destination,
        &ExecutionCancellationToken::new(),
    );
    eprintln!(
        "HELD_FEE_PUBLIC_PREPARE: accepted={}; error={:?}",
        result.is_ok(),
        result.as_ref().err()
    );
    let prepared = result.expect("actual public source must qualify this successful preparation");
    assert_eq!(fixture.inner.runtime.resource_root_identity(), root);
    assert_eq!(fixture.inner.runtime.resource_footprint(), footprint);
    let after = preflight_cursors(&fixture.inner.runtime);
    assert_eq!(after.sessions, cursors.sessions);
    assert_eq!(after.processors, cursors.processors);
    assert_eq!(
        fixture.inner.source.current_claim().unwrap().unwrap().generation(),
        claim.generation()
    );
    (prepared, authorization)
}

fn fee_public_install(
    fixture: &mut PreflightFixture,
    prepared: &mut crate::PreparedNativeAudioParameterHandoff,
    authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
) {
    let before = preflight_cursors(&fixture.inner.runtime);
    let creates = fixture.registry.calls.creates.load(Ordering::Relaxed);
    let object = fixture.inner.probe.lock().unwrap().binding.unwrap().expected_object;
    let entries = fixture.inner.probe.lock().unwrap().entries;
    let receipt = fixture
        .inner
        .runtime
        .install_native_author_parameter_handoff(
            prepared,
            authorization,
            &ExecutionCancellationToken::new(),
        )
        .expect("valid public Source authorization installs the actual owner");
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
    let after = preflight_cursors(&fixture.inner.runtime);
    assert_eq!(after.sessions.len(), before.sessions.len());
    assert_eq!(after.processors.len(), before.processors.len());
    for (old, new) in before.sessions.iter().zip(&after.sessions) {
        assert_eq!(new.identity.session_id, old.identity.session_id);
        assert_eq!(new.state, old.state);
    }
    for (old, new) in before.processors.iter().zip(&after.processors) {
        assert_eq!(new.occurrence, old.occurrence);
        assert_eq!(new.state, old.state);
    }
    assert_eq!(
        fixture.registry.calls.creates.load(Ordering::Relaxed),
        creates
    );
    let probe = fixture.inner.probe.lock().unwrap();
    assert_eq!(probe.binding.unwrap().expected_object, object);
    assert_eq!(probe.entries, entries);
    drop(probe);
    fixture.inner.target = receipt.target;
}

fn fee_public_wrong_authorization() -> (
    mondrian_core::AudioOwnerInstallationSource,
    mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
) {
    let foreign = mondrian_core::AudioOwnerInstallationSource::new();
    let (registration, _) = foreign.register().unwrap();
    let reservation = foreign.reserve_install(&registration, None).unwrap();
    let authorization = foreign.authorize_runtime_installation(reservation).unwrap();
    (foreign, authorization)
}

fn fee_public_constant_weight() -> usize {
    let spec = preflight_spec(PreflightPlacement::Output);
    let mut control = preflight_build(&spec, usize::MAX);
    let old = control.inner.runtime.resource_footprint().prepared_logical_bytes;
    let proposal = control.inner.value(0.75);
    let programs = preflight_programs(&mut control, 0.75);
    preflight_install(&mut control, &proposal, &programs);
    let next = control.inner.runtime.resource_footprint().prepared_logical_bytes;
    assert!(old > 0);
    assert_eq!(
        old, next,
        "actual constant fixture must first qualify its equal policy weights"
    );
    old
}

#[test]
fn native_author_pending_fees_survive_entry_cancel_and_failed_source_install() {
    let weight = fee_public_constant_weight();
    let grant = weight.checked_mul(3).unwrap();
    let mut fixture = preflight_build(&preflight_spec(PreflightPlacement::Output), grant);
    let frozen = fixture.inner.runtime.resource_grant();
    let proposal = fixture.inner.value(0.75);
    let programs = preflight_programs(&mut fixture, 0.75);
    let (first, _) = fee_public_prepare(&mut fixture, &proposal, &programs);
    let (_, cancelled_destination) = fixture.inner.source.register().unwrap();
    let cancellation = ExecutionCancellationToken::new();
    cancellation.cancel();
    let calls = fixture.registry.calls.snapshot();
    let cancelled = fixture.inner.runtime.prepare_native_author_parameter_handoff(
        &programs,
        &proposal,
        cancelled_destination,
        &cancellation,
    );
    assert!(matches!(
        cancelled,
        Err(NativeAudioParameterHandoffError::Admission(
            NativeAudioAuthorAdmissionError::Cancelled
        ))
    ));
    assert_eq!(fixture.registry.calls.snapshot(), calls);
    // Cancellation at API entry must not consume credit for the actual first owner.
    let (mut second, _) = fee_public_prepare(&mut fixture, &proposal, &programs);
    preflight_assert_early_denial(
        &mut fixture,
        &proposal,
        &programs,
        weight.checked_mul(4).unwrap(),
        grant,
    );
    let root = fixture.inner.runtime.resource_root_identity();
    let footprint = fixture.inner.runtime.resource_footprint();
    let claim = fixture.inner.source.current_claim().unwrap().unwrap();
    let (_foreign, mut wrong_authorization) = fee_public_wrong_authorization();
    let wrong = fixture.inner.runtime.install_native_author_parameter_handoff(
        &mut second,
        &mut wrong_authorization,
        &ExecutionCancellationToken::new(),
    );
    assert!(matches!(
        wrong,
        Err(NativeAudioParameterHandoffError::Installation(_))
    ));
    assert_eq!(fixture.inner.runtime.resource_root_identity(), root);
    assert_eq!(fixture.inner.runtime.resource_footprint(), footprint);
    assert_eq!(
        fixture.inner.source.current_claim().unwrap().unwrap().generation(),
        claim.generation()
    );
    preflight_assert_early_denial(
        &mut fixture,
        &proposal,
        &programs,
        weight.checked_mul(4).unwrap(),
        grant,
    );
    drop(first);
    let (third, _) = fee_public_prepare(&mut fixture, &proposal, &programs);
    assert_eq!(fixture.inner.runtime.resource_grant(), frozen);
    drop(third);
    drop(second);
    let (retry, _) = fee_public_prepare(&mut fixture, &proposal, &programs);
    drop(retry);
}

#[test]
fn native_author_stale_pending_fee_survives_install_and_exact_ack() {
    let weight = fee_public_constant_weight();
    let grant = weight.checked_mul(3).unwrap();
    let mut fixture = preflight_build(&preflight_spec(PreflightPlacement::Output), grant);
    let frozen = fixture.inner.runtime.resource_grant();
    let proposal = fixture.inner.value(0.75);
    let programs = preflight_programs(&mut fixture, 0.75);
    let (mut first, mut first_authorization) =
        fee_public_prepare(&mut fixture, &proposal, &programs);
    let old_root = fixture.inner.runtime.resource_root_identity();
    let old_weak = Arc::downgrade(
        fixture
            .inner
            .runtime
            .find_resource_session(old_root)
            .unwrap()
            .shared_resource_plan(),
    );
    let (mut second, mut second_authorization) =
        fee_public_prepare(&mut fixture, &proposal, &programs);
    // The second Bind is the real native object's latest acknowledged binding.
    fee_public_install(&mut fixture, &mut second, &mut second_authorization);
    fixture.inner.ack(&proposal);
    assert!(
        old_weak.strong_count() > 0,
        "second handle actually retains old installed Plan after Ack"
    );
    fixture.inner.block(1, 0.75);
    let suffix = fixture.inner.value(0.25);
    let suffix_programs = preflight_programs(&mut fixture, 0.25);
    preflight_assert_early_denial(
        &mut fixture,
        &suffix,
        &suffix_programs,
        weight.checked_mul(4).unwrap(),
        grant,
    );
    let stale = fixture.inner.runtime.install_native_author_parameter_handoff(
        &mut first,
        &mut first_authorization,
        &ExecutionCancellationToken::new(),
    );
    assert!(matches!(
        stale,
        Err(NativeAudioParameterHandoffError::Owner(
            AudioNativeOwnerError::OwnerUnavailable
        ))
    ));
    preflight_assert_early_denial(
        &mut fixture,
        &suffix,
        &suffix_programs,
        weight.checked_mul(4).unwrap(),
        grant,
    );
    drop(second);
    assert!(
        old_weak.upgrade().is_none(),
        "accounting keeps no strong Plan registry after the real retired owner drops"
    );
    let (third, _) = fee_public_prepare(&mut fixture, &suffix, &suffix_programs);
    assert_eq!(fixture.inner.runtime.resource_grant(), frozen);
    drop(third);
    drop(first);
}

#[test]
fn native_author_two_retired_handles_share_one_original_root_grant() {
    let weight = fee_public_constant_weight();
    let grant = weight.checked_mul(3).unwrap();
    let mut fixture = preflight_build(&preflight_spec(PreflightPlacement::Output), grant);
    let frozen = fixture.inner.runtime.resource_grant();
    let initial = fixture.inner.runtime.resource_root_identity();
    let initial_weak = Arc::downgrade(
        fixture
            .inner
            .runtime
            .find_resource_session(initial)
            .unwrap()
            .shared_resource_plan(),
    );
    let first_proposal = fixture.inner.value(0.75);
    let first_programs = preflight_programs(&mut fixture, 0.75);
    let (mut first, mut first_authorization) =
        fee_public_prepare(&mut fixture, &first_proposal, &first_programs);
    fee_public_install(&mut fixture, &mut first, &mut first_authorization);
    fixture.inner.ack(&first_proposal);
    fixture.inner.block(1, 0.75);
    let next = fixture.inner.runtime.resource_root_identity();
    let next_weak = Arc::downgrade(
        fixture
            .inner
            .runtime
            .find_resource_session(next)
            .unwrap()
            .shared_resource_plan(),
    );
    let second_proposal = fixture.inner.value(0.25);
    let second_programs = preflight_programs(&mut fixture, 0.25);
    let (mut second, mut second_authorization) =
        fee_public_prepare(&mut fixture, &second_proposal, &second_programs);
    fee_public_install(&mut fixture, &mut second, &mut second_authorization);
    fixture.inner.ack(&second_proposal);
    fixture.inner.block(2, 0.25);
    assert!(initial_weak.strong_count() > 0);
    assert!(next_weak.strong_count() > 0);
    let third_proposal = fixture.inner.value(0.5);
    let third_programs = preflight_programs(&mut fixture, 0.5);
    preflight_assert_early_denial(
        &mut fixture,
        &third_proposal,
        &third_programs,
        weight.checked_mul(4).unwrap(),
        grant,
    );
    drop(first);
    assert!(initial_weak.upgrade().is_none());
    assert!(
        next_weak.strong_count() > 0,
        "dropping one actual retired owner cannot release the other fee"
    );
    let (mut third, mut third_authorization) =
        fee_public_prepare(&mut fixture, &third_proposal, &third_programs);
    // The surviving old owner plus this new pending owner still fill 3L.
    // A refund that incorrectly reset all H would otherwise pass the retry.
    preflight_assert_early_denial(
        &mut fixture,
        &third_proposal,
        &third_programs,
        weight.checked_mul(4).unwrap(),
        grant,
    );
    fee_public_install(&mut fixture, &mut third, &mut third_authorization);
    // Release the newly retired owners outside final guards before ordinary Ack.
    drop(third);
    fixture.inner.ack(&third_proposal);
    fixture.inner.block(3, 0.5);
    assert_eq!(fixture.inner.runtime.resource_grant(), frozen);
    assert!(next_weak.strong_count() > 0);
    drop(second);
    assert!(next_weak.upgrade().is_none());
}

fn fee_public_curve(value: f64, keys: usize) -> ExactAutomationCurve {
    let mut curve =
        ExactAutomationCurve::new(ParameterId::new_static("native.gain"), value).unwrap();
    for index in 0..keys {
        curve
            .set_keyframe(mondrian_core::ExactAutomationKeyframe::linear(
                tt(i64::try_from(index).unwrap(), 2),
                value,
            ))
            .unwrap();
    }
    curve
}

fn fee_public_output_spec(keys: usize) -> PreflightSpec {
    let mut spec = preflight_spec(PreflightPlacement::Output);
    spec.canonical.audio_program.outputs[0].strip.pre_fader.processors[0]
        .set_parameter_automation(fee_public_curve(0.5, keys))
        .unwrap();
    spec.root = spec.canonical.clone();
    spec
}

fn fee_public_output_programs(
    fixture: &mut PreflightFixture,
    value: f64,
    keys: usize,
) -> [NativeAudioAuthorProgram; 1] {
    fixture.inner.sequence.audio_program.outputs[0].strip.pre_fader.processors[0]
        .set_parameter_automation(fee_public_curve(value, keys))
        .unwrap();
    [NativeAudioAuthorProgram {
        sequence: fixture.inner.sequence.id,
        program: Arc::new(
            compile_audio_program(
                &fixture.inner.sequence,
                AudioCompileRequest::program(fixture.inner.sequence.audio_program.outputs[0].id),
            )
            .unwrap(),
        ),
    }]
}

fn fee_public_unequal_transfer(old_keys: usize, next_keys: usize, growth: bool) {
    let spec = fee_public_output_spec(old_keys);
    let (old, next) = {
        let mut control = preflight_build(&spec, usize::MAX);
        let old = control.inner.runtime.resource_footprint().prepared_logical_bytes;
        let proposal = control.inner.value(0.75);
        let programs = fee_public_output_programs(&mut control, 0.75, next_keys);
        preflight_install(&mut control, &proposal, &programs);
        let next = control.inner.runtime.resource_footprint().prepared_logical_bytes;
        (old, next)
    };
    assert!(old > 0 && next > 0);
    if growth {
        assert!(next > old, "actual keyed Program must qualify growth");
    } else {
        assert!(old > next, "actual keyed Program must qualify shrink");
    }
    let first_peak = old.checked_add(next).unwrap();
    let retry_peak = next.checked_mul(2).unwrap();
    let grant = first_peak.max(retry_peak);
    let denied_peak = old.checked_add(retry_peak).unwrap();
    assert!(denied_peak > grant);
    let mut fixture = preflight_build(&spec, grant);
    let frozen = fixture.inner.runtime.resource_grant();
    let proposal = fixture.inner.value(0.75);
    let programs = fee_public_output_programs(&mut fixture, 0.75, next_keys);
    let (mut prepared, mut authorization) = fee_public_prepare(&mut fixture, &proposal, &programs);
    let (_foreign, mut wrong_authorization) = fee_public_wrong_authorization();
    let wrong = fixture.inner.runtime.install_native_author_parameter_handoff(
        &mut prepared,
        &mut wrong_authorization,
        &ExecutionCancellationToken::new(),
    );
    assert!(matches!(
        wrong,
        Err(NativeAudioParameterHandoffError::Installation(_))
    ));
    // Before CAS: current O + pending N + prospective N. Failure cannot credit N.
    preflight_assert_early_denial(&mut fixture, &proposal, &programs, denied_peak, grant);
    fee_public_install(&mut fixture, &mut prepared, &mut authorization);
    assert_eq!(
        fixture.inner.runtime.resource_footprint().prepared_logical_bytes,
        next
    );
    fixture.inner.probe.lock().unwrap().reject_ack = true;
    assert!(fixture
        .inner
        .runtime
        .acknowledge_native_author_parameter_handoff(&proposal, &ExecutionCancellationToken::new(),)
        .is_err());
    fixture.inner.probe.lock().unwrap().reject_ack = false;
    fixture.inner.ack(&proposal);
    fixture.inner.block(1, 0.75);
    let suffix = fixture.inner.value(0.25);
    let suffix_programs = fee_public_output_programs(&mut fixture, 0.25, next_keys);
    // After CAS: current N + retired O + prospective N, the same exact peak.
    preflight_assert_early_denial(&mut fixture, &suffix, &suffix_programs, denied_peak, grant);
    let duplicate = fixture.inner.runtime.install_native_author_parameter_handoff(
        &mut prepared,
        &mut authorization,
        &ExecutionCancellationToken::new(),
    );
    assert!(matches!(
        duplicate,
        Err(NativeAudioParameterHandoffError::Owner(
            AudioNativeOwnerError::OwnerUnavailable
        ))
    ));
    preflight_assert_early_denial(&mut fixture, &suffix, &suffix_programs, denied_peak, grant);
    drop(prepared);
    let (retry, _) = fee_public_prepare(&mut fixture, &suffix, &suffix_programs);
    assert_eq!(fixture.inner.runtime.resource_grant(), frozen);
    drop(retry);
}

#[test]
fn native_author_logical_fee_growth_moves_pending_next_to_retired_old() {
    fee_public_unequal_transfer(0, 9, true);
}

#[test]
fn native_author_logical_fee_shrink_moves_pending_next_to_retired_old() {
    fee_public_unequal_transfer(9, 0, false);
}
