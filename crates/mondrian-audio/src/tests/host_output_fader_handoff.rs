//! Portable real Runtime/Source/Session contract, not third-party SDK evidence.

use super::*;

fn closed_fixture() -> HandoffFixture {
    let mut fixture = HandoffFixture::new();
    assert!(matches!(
        fixture
            .runtime
            .control_native_editor(
                Intent::Close { target: fixture.target },
                &ExecutionCancellationToken::new(),
            )
            .unwrap(),
        Outcome::Closed { .. }
    ));
    fixture.block(0, 0.5);
    fixture
}

fn next_program(fixture: &HandoffFixture) -> Arc<CompiledAudioProgram> {
    let mut sequence = fixture.sequence.clone();
    sequence.audio_program.outputs[0].strip.fader_db = -6.020_599_913_279_624;
    Arc::new(
        compile_audio_program(
            &sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
        )
        .unwrap(),
    )
}

fn reserve(
    fixture: &HandoffFixture,
) -> (
    mondrian_core::AudioOwnerInstallationVerifier,
    mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
) {
    let (registration, verifier) = fixture.source.register().unwrap();
    let old = fixture.source.current_claim().unwrap().unwrap();
    let reservation = fixture.source.reserve_install(&registration, Some(&old)).unwrap();
    (
        verifier,
        fixture.source.authorize_runtime_installation(reservation).unwrap(),
    )
}

fn counts(fixture: &HandoffFixture) -> (usize, usize, usize, usize) {
    let probe = fixture.probe.lock().unwrap();
    (
        probe.creates,
        probe.binds,
        probe.entries,
        probe.accepted_acks,
    )
}

#[test]
fn host_fader_preserves_session_epoch_native_binding_and_exact_frontier() {
    let mut fixture = closed_fixture();
    let prior = fixture.target;
    let old_counts = counts(&fixture);
    let old_claim = fixture.source.current_claim().unwrap().unwrap();
    let program = next_program(&fixture);
    let (destination, mut authorization) = reserve(&fixture);
    let mut prepared = fixture
        .runtime
        .prepare_host_output_fader_handoff(
            program,
            destination.clone(),
            &ExecutionCancellationToken::new(),
        )
        .unwrap();
    assert_eq!(
        counts(&fixture),
        old_counts,
        "preparation cannot Bind/Enter/Ack/create"
    );
    let receipt = fixture
        .runtime
        .install_host_output_fader_handoff(
            &mut prepared,
            &mut authorization,
            &ExecutionCancellationToken::new(),
        )
        .unwrap();
    assert_eq!(
        receipt.identity.session_id,
        receipt.frontier.identity.session_id
    );
    assert_ne!(
        receipt.identity.prepared_plan_id,
        receipt.frontier.identity.prepared_plan_id
    );
    assert!(
        matches!(receipt.frontier.state, AudioSessionCursorState::Active {
        epoch, next_public_sample, next_execution_sample, alignment_primed: true,
    } if epoch == AudioContinuityEpoch::new(47) && next_public_sample.sample() == 1 && next_execution_sample.sample() == 1)
    );
    assert!(destination.validate(&old_claim).is_err());
    destination.validate(&receipt.claim).unwrap();
    assert_eq!(
        counts(&fixture),
        old_counts,
        "publication cannot Bind/Enter/Ack/create"
    );
    let mut address = crate::native_runtime_owner::editor_target_address(prior);
    address.root = receipt.identity;
    address.local = receipt.identity;
    let current = fixture.runtime.native_editor_target(address).unwrap();
    assert_eq!(current.binding, prior.binding);
    assert_eq!(current.local_session, prior.local_session);
    assert_eq!(current.continuity_epoch, prior.continuity_epoch);
    assert_ne!(current.local_plan, prior.local_plan);
    assert_eq!(current.installed_generation, receipt.claim.generation());
    assert!(
        matches!(
            fixture
                .runtime
                .control_native_editor(
                    Intent::Close { target: prior },
                    &ExecutionCancellationToken::new(),
                )
                .unwrap(),
            Outcome::Refused(_)
        ),
        "old full parent target must be rejected"
    );
    drop(prepared);
    let mut pcm = [91.0];
    fixture
        .runtime
        .render_into(AudioRenderRequest { start_sample: 1, frames: 1 }, &mut pcm)
        .unwrap();
    assert!(
        (pcm[0] - 0.5).abs() < 1e-6,
        "new Host gain at F, original native gain unchanged"
    );
    assert_eq!(counts(&fixture), old_counts);
    assert_eq!(
        fixture.probe.lock().unwrap().processed,
        vec![(0, 0.5), (1, 0.5)]
    );
}

#[test]
fn host_fader_stale_frontier_cancel_and_source_cas_refuse_before_swap() {
    for scenario in 0..3 {
        let mut fixture = closed_fixture();
        let program = next_program(&fixture);
        let old_root = fixture.runtime.resource_root_identity();
        let old_counts = counts(&fixture);
        let (destination, mut authorization) = reserve(&fixture);
        let mut prepared = fixture
            .runtime
            .prepare_host_output_fader_handoff(
                program,
                destination,
                &ExecutionCancellationToken::new(),
            )
            .unwrap();
        let cancellation = ExecutionCancellationToken::new();
        match scenario {
            0 => fixture.block(1, 0.5), // Actual DSP advanced; not just an identity mismatch.
            1 => cancellation.cancel(),
            2 => {
                let (superseding_verifier, mut superseding) = reserve(&fixture);
                superseding.begin_commit(&superseding_verifier).unwrap().commit();
            }
            _ => unreachable!(),
        }
        assert!(fixture
            .runtime
            .install_host_output_fader_handoff(&mut prepared, &mut authorization, &cancellation,)
            .is_err());
        assert_eq!(fixture.runtime.resource_root_identity(), old_root);
        assert_eq!(counts(&fixture), old_counts);
        drop(prepared);
        if scenario < 2 {
            fixture.block(if scenario == 0 { 2 } else { 1 }, 0.5);
        }
    }
}

#[test]
fn host_fader_refuses_active_editor_pending_source_and_unrelated_math() {
    for scenario in 0..6 {
        let mut fixture = if scenario == 0 {
            let mut fixture = HandoffFixture::new();
            fixture.block(0, 0.5);
            fixture
        } else {
            closed_fixture()
        };
        let mut program = next_program(&fixture);
        match scenario {
            1 => fixture.probe.lock().unwrap().open = 1,
            2 => fixture.queue_value(0.75),
            3 => Arc::make_mut(&mut program).output.input_trim_db = -3.0,
            4 => {
                fixture.probe.lock().unwrap().live_request =
                    Some(crate::NativeObservedRequest::RequestOnly(
                        mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
                    ))
            }
            5 => {
                fixture.probe.lock().unwrap().source_failure =
                    Some(crate::NativeAudioAuthorSourceFailure::Overflow)
            }
            _ => {}
        }
        let root = fixture.runtime.resource_root_identity();
        let old_counts = counts(&fixture);
        let (destination, _) = reserve(&fixture);
        assert!(fixture
            .runtime
            .prepare_host_output_fader_handoff(
                program,
                destination,
                &ExecutionCancellationToken::new(),
            )
            .is_err());
        assert_eq!(fixture.runtime.resource_root_identity(), root);
        assert_eq!(counts(&fixture), old_counts);
    }
}
