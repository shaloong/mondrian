//! Portable original Runtime/Source/Hosted batch contracts; not native DLL proof.
use super::*;

struct Fixture {
    owned: PreflightFixture,
    members: [mondrian_core::NativeAudioAuthorProposal; 2],
    programs: [NativeAudioAuthorProgram; 1],
    probes: Vec<Arc<Mutex<HandoffProbe>>>,
}

fn fixture(placement: PreflightPlacement) -> Fixture {
    fixture_completion(placement, true)
}

fn fixture_completion(placement: PreflightPlacement, terminal: bool) -> Fixture {
    let mut spec = preflight_spec(placement);
    if matches!(placement, PreflightPlacement::Output) {
        let rack = &mut spec.canonical.audio_program.outputs[0].strip.pre_fader;
        let mut peer = rack.processors[0].clone();
        peer.id = mondrian_core::AudioProcessorInstanceId::new();
        rack.processors.push(peer);
        spec.root = spec.canonical.clone();
    }
    let mut owned = preflight_build_with_pcm(
        &spec,
        64 * 1024 * 1024,
        if matches!(placement, PreflightPlacement::Output) {
            0.25
        } else {
            1.0
        },
    );
    let probes = owned.registry.probes.lock().unwrap().clone();
    assert_eq!(probes.len(), 2);
    for probe in &probes {
        let mut source = probe.lock().unwrap();
        assert!(source.pending.is_empty() && source.retained.is_none());
        assert_eq!(source.open, 0);
        // The existing processor's explicit ABI owns this complete control
        // callback batch and coherent scalar bytes; no Core proposal is minted.
        for (phase, value) in [
            (NativeAudioAuthorParameterPhase::Begin, None),
            (NativeAudioAuthorParameterPhase::Value, Some(0.75)),
            (NativeAudioAuthorParameterPhase::End, None),
        ] {
            source.revision += 1;
            source.pending.push(NativeAudioAuthorEvent::Parameter(
                NativeAudioAuthorParameterEvent::admit(
                    ParameterId::new_static("native.gain"),
                    phase,
                    value.map(|value| NativeAudioAuthorScalar::admit(value).unwrap()),
                )
                .unwrap(),
            ));
        }
        source.configuration = terminal.then(|| 0.75_f64.to_le_bytes().to_vec());
    }
    let mut delivered = Vec::with_capacity(2);
    for _ in 0..2 {
        let batch = owned
            .inner
            .runtime
            .poll_native_observations(&ExecutionCancellationToken::new())
            .unwrap()
            .unwrap();
        let proposal = batch.author.expect("actual next Hosted occurrence's retained delivery");
        assert_eq!(proposal.terminal(), terminal);
        delivered.push(proposal);
    }
    let members: [_; 2] = delivered.try_into().unwrap();
    assert_ne!(members[0].target(), members[1].target());
    let mut candidate = spec.canonical.clone();
    let audio = &mut candidate.audio_program;
    for processor in audio.outputs[0].strip.pre_fader.processors.iter_mut().chain(
        audio
            .processing_scopes
            .iter_mut()
            .flat_map(|scope| scope.processors.processors.iter_mut()),
    ) {
        if let Some(member) = members.iter().find(|member| member.target().instance == processor.id)
        {
            processor.opaque_state = member.state().cloned();
            processor
                .set_parameter_automation(
                    ExactAutomationCurve::new(ParameterId::new_static("native.gain"), 0.75)
                        .unwrap(),
                )
                .unwrap();
        }
    }
    let programs = [NativeAudioAuthorProgram {
        sequence: candidate.id,
        program: Arc::new(
            compile_audio_program(
                &candidate,
                AudioCompileRequest::program(candidate.audio_program.outputs[0].id),
            )
            .unwrap(),
        ),
    }];
    Fixture { owned, members, programs, probes }
}

fn forged_equal(
    member: &mondrian_core::NativeAudioAuthorProposal,
) -> mondrian_core::NativeAudioAuthorProposal {
    mondrian_core::NativeAudioAuthorProposal::project(
        member.target(),
        member.sequence(),
        member.revision(),
        mondrian_core::NativeAudioAuthorBoundaryFacts {
            completion: member.completion(),
            open_parameter_edits: member.open_parameter_edits(),
            group_depth: member.group_depth(),
        },
        member.callbacks().to_vec(),
        member.state().map(|bytes| bytes.as_slice().to_vec()),
    )
    .unwrap()
}

fn ack_counts(probe: &Arc<Mutex<HandoffProbe>>) -> (usize, usize) {
    let probe = probe.lock().unwrap();
    (probe.ack_attempts, probe.accepted_acks)
}

#[test]
fn terminal_batch_rejects_incomplete_unowned_and_state_entry_sets_before_bind() {
    let mut f = fixture(PreflightPlacement::Output);
    let foreign = fixture(PreflightPlacement::Output);
    let fake = forged_equal(&f.members[0]);
    assert_eq!(fake, f.members[0]);
    assert!(!fake.same_retained_proposal(&f.members[0]));
    let (_, destination) = f.owned.inner.source.register().unwrap();
    let before = preflight_cursors(&f.owned.inner.runtime);
    let claim = f.owned.inner.source.current_claim().unwrap().unwrap();
    let binds = f.owned.registry.calls.binds.load(Ordering::Relaxed);
    for invalid in [
        Vec::new(),
        vec![f.members[0].clone()],
        vec![f.members[0].clone(), f.members[0].clone()],
        vec![f.members[0].clone(), foreign.members[1].clone()],
        vec![fake, f.members[1].clone()],
    ] {
        assert!(f
            .owned
            .inner
            .runtime
            .prepare_native_author_terminal_batch(
                &f.programs,
                &invalid,
                destination.clone(),
                &ExecutionCancellationToken::new()
            )
            .is_err());
        assert_eq!(f.owned.registry.calls.binds.load(Ordering::Relaxed), binds);
        assert_eq!(preflight_cursors(&f.owned.inner.runtime), before);
        assert_eq!(
            f.owned.inner.source.current_claim().unwrap().unwrap().generation(),
            claim.generation()
        );
    }
    let mut nonterminal = fixture_completion(PreflightPlacement::Output, false);
    let (_, rejected_destination) = nonterminal.owned.inner.source.register().unwrap();
    let no_calls = nonterminal.owned.registry.calls.snapshot();
    assert!(nonterminal
        .owned
        .inner
        .runtime
        .prepare_native_author_terminal_batch(
            &nonterminal.programs,
            &nonterminal.members,
            rejected_destination,
            &ExecutionCancellationToken::new()
        )
        .is_err());
    assert_eq!(
        nonterminal.owned.registry.calls.snapshot(),
        no_calls,
        "an actual nonterminal prefix does not acquire this terminal-only policy"
    );
    for placement in [
        PreflightPlacement::ScopeTwoContributions,
        PreflightPlacement::NestedTwoPlacements,
    ] {
        let mut blocked = fixture(placement);
        let (_, destination) = blocked.owned.inner.source.register().unwrap();
        let before = blocked.owned.registry.calls.snapshot();
        assert!(blocked
            .owned
            .inner
            .runtime
            .prepare_native_author_terminal_batch(
                &blocked.programs,
                &blocked.members,
                destination,
                &ExecutionCancellationToken::new()
            )
            .is_err());
        assert_eq!(
            blocked.owned.registry.calls.snapshot(),
            before,
            "unsupported actual state-entry/nested owners reject before native calls"
        );
    }
    for probe in &f.probes {
        assert!(probe.lock().unwrap().retained.is_some());
    }
}

#[test]
fn terminal_batch_final_recheck_guard_and_partial_ack_keep_original_custody() {
    let mut f = fixture(PreflightPlacement::Output);
    let (registration, destination) = f.owned.inner.source.register().unwrap();
    let claim = f.owned.inner.source.current_claim().unwrap().unwrap();
    let before = preflight_cursors(&f.owned.inner.runtime);
    let cancel = ExecutionCancellationToken::new();
    let mut prepared = f
        .owned
        .inner
        .runtime
        .prepare_native_author_terminal_batch(&f.programs, &f.members, destination, &cancel)
        .unwrap();
    let reservation = f.owned.inner.source.reserve_install(&registration, Some(&claim)).unwrap();
    let mut authorization =
        f.owned.inner.source.authorize_runtime_installation(reservation).unwrap();
    f.probes[1].lock().unwrap().source_failure =
        Some(crate::NativeAudioAuthorSourceFailure::WrongThread);
    let acquired = AtomicUsize::new(0);
    assert!(f
        .owned
        .inner
        .runtime
        .install_native_author_terminal_batch_with_commit_guard(
            &mut prepared,
            &mut authorization,
            &cancel,
            || {
                acquired.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        )
        .is_err());
    assert_eq!(acquired.load(Ordering::Relaxed), 0);
    assert_eq!(preflight_cursors(&f.owned.inner.runtime), before);
    assert_eq!(
        f.owned.inner.source.current_claim().unwrap().unwrap().generation(),
        claim.generation()
    );
    for member in &f.members {
        assert!(prepared.installed_target(member).is_none());
    }
    // Repair only the model's deliberate corrupt response. No prefix/binding,
    // canonical data, native suffix or Source installation is cleared.
    f.probes[1].lock().unwrap().source_failure = None;
    struct Guard {
        registry: Arc<PreflightRegistry>,
        calls: [usize; 3],
        alive: Arc<AtomicUsize>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            assert_eq!(
                self.registry.calls.snapshot(),
                self.calls,
                "all native observations/Bind/create precede the canonical guard"
            );
            self.alive.store(0, Ordering::Relaxed);
        }
    }
    let alive = Arc::new(AtomicUsize::new(0));
    let registry = Arc::clone(&f.owned.registry);
    let (installed, guard) = f
        .owned
        .inner
        .runtime
        .install_native_author_terminal_batch_with_commit_guard(
            &mut prepared,
            &mut authorization,
            &cancel,
            || {
                alive.store(1, Ordering::Relaxed);
                Ok(Guard {
                    calls: registry.calls.snapshot(),
                    registry,
                    alive: Arc::clone(&alive),
                })
            },
        )
        .unwrap();
    assert_eq!(alive.load(Ordering::Relaxed), 1);
    assert_ne!(installed.claim.generation(), claim.generation());
    for member in &f.members {
        assert!(prepared.installed_target(member).is_some());
    }
    assert!(prepared.installed_target(&forged_equal(&f.members[0])).is_none());
    drop(guard);
    assert_eq!(alive.load(Ordering::Relaxed), 0);
    let probe_for = |member: &mondrian_core::NativeAudioAuthorProposal| {
        f.probes
            .iter()
            .find(|probe| {
                probe.lock().unwrap().retained.unwrap().0.binding == member.target().binding
            })
            .unwrap()
            .clone()
    };
    let a = probe_for(&f.members[0]);
    let b = probe_for(&f.members[1]);
    f.owned
        .inner
        .runtime
        .acknowledge_native_author_parameter_handoff(&f.members[0], &cancel)
        .unwrap();
    b.lock().unwrap().reject_ack = true;
    assert!(f
        .owned
        .inner
        .runtime
        .acknowledge_native_author_parameter_handoff(&f.members[1], &cancel)
        .is_err());
    assert_eq!(ack_counts(&a), (1, 1));
    assert_eq!(ack_counts(&b), (1, 0));
    let installed_cursors = preflight_cursors(&f.owned.inner.runtime);
    let mut pcm = [91.0];
    assert!(matches!(
        f.owned
            .inner
            .runtime
            .render_into(AudioRenderRequest { start_sample: 1, frames: 1 }, &mut pcm),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(pcm, [91.0]);
    assert_eq!(preflight_cursors(&f.owned.inner.runtime), installed_cursors);
    {
        let mut b = b.lock().unwrap();
        b.reject_ack = false;
        b.revision += 1;
        b.pending.push(NativeAudioAuthorEvent::Parameter(
            NativeAudioAuthorParameterEvent::admit(
                ParameterId::new_static("native.gain"),
                NativeAudioAuthorParameterPhase::Value,
                Some(NativeAudioAuthorScalar::admit(0.875).unwrap()),
            )
            .unwrap(),
        ));
    }
    let cancelled = ExecutionCancellationToken::new();
    cancelled.cancel();
    assert!(f
        .owned
        .inner
        .runtime
        .acknowledge_native_author_parameter_handoff(&f.members[1], &cancelled)
        .is_err());
    assert_eq!(b.lock().unwrap().ack_attempts, 1);
    let last = f
        .owned
        .inner
        .runtime
        .acknowledge_native_author_parameter_handoff(&f.members[1], &cancel)
        .unwrap();
    assert_eq!(last.claim.generation(), installed.claim.generation());
    assert_eq!(ack_counts(&a), (1, 1));
    assert_eq!(ack_counts(&b), (2, 1));
    assert_eq!(
        b.lock().unwrap().pending.len(),
        1,
        "original prefix ACK never consumes its later suffix"
    );
    assert!(f.owned.inner.runtime.validate_native_author_source_closed(&cancel).is_err());
    assert_eq!(
        f.owned.inner.source.current_claim().unwrap().unwrap().generation(),
        installed.claim.generation()
    );
    assert_eq!(preflight_cursors(&f.owned.inner.runtime), installed_cursors);
    drop(prepared);
}

#[test]
fn terminal_batch_source_supersession_never_acquires_guard_or_consumes_retained_members() {
    let mut f = fixture(PreflightPlacement::Output);
    let (registration, destination) = f.owned.inner.source.register().unwrap();
    let old = f.owned.inner.source.current_claim().unwrap().unwrap();
    let cancel = ExecutionCancellationToken::new();
    let mut prepared = f
        .owned
        .inner
        .runtime
        .prepare_native_author_terminal_batch(&f.programs, &f.members, destination, &cancel)
        .unwrap();
    let reservation = f.owned.inner.source.reserve_install(&registration, Some(&old)).unwrap();
    let mut authorization =
        f.owned.inner.source.authorize_runtime_installation(reservation).unwrap();
    let (replacement, _) = f.owned.inner.source.register().unwrap();
    let new_claim = f
        .owned
        .inner
        .source
        .begin_commit(f.owned.inner.source.reserve_install(&replacement, Some(&old)).unwrap())
        .unwrap()
        .commit()
        .unwrap();
    let calls = f.owned.registry.calls.snapshot();
    let before = preflight_cursors(&f.owned.inner.runtime);
    let acquired = AtomicUsize::new(0);
    assert!(f
        .owned
        .inner
        .runtime
        .install_native_author_terminal_batch_with_commit_guard(
            &mut prepared,
            &mut authorization,
            &cancel,
            || {
                acquired.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        )
        .is_err());
    assert_eq!(acquired.load(Ordering::Relaxed), 0);
    assert_eq!(f.owned.registry.calls.snapshot(), calls);
    assert_eq!(preflight_cursors(&f.owned.inner.runtime), before);
    assert_eq!(
        f.owned.inner.source.current_claim().unwrap().unwrap().generation(),
        new_claim.generation()
    );
    for member in &f.members {
        assert!(prepared.installed_target(member).is_none());
    }
    for probe in &f.probes {
        let probe = probe.lock().unwrap();
        assert!(probe.retained.is_some());
        assert_eq!(probe.ack_attempts, 0);
    }
    drop(prepared); // Original producer-domain owner cleanup, never a compensating ACK.
}
