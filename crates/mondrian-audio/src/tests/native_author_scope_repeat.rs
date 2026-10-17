//! Repeated Preserved parameter handoff through two actual Scope occurrences.
//! Published resource metadata is not an allocator-capacity receipt.

use super::*;

fn scope_repeat_assert_preserved_cursors(old: &PreflightCursorSet, next: &PreflightCursorSet) {
    assert_eq!(
        old.sessions.len(),
        1,
        "this control has one actual Root Session"
    );
    assert_eq!(next.sessions.len(), old.sessions.len());
    assert_eq!(
        old.processors.len(),
        2,
        "both Scope materializations are actual processors"
    );
    assert_eq!(next.processors.len(), old.processors.len());
    for previous in &old.sessions {
        let current = next
            .sessions
            .iter()
            .find(|current| current.identity.session_id == previous.identity.session_id)
            .expect("same exclusive Session survives publication");
        assert_ne!(
            current.identity.prepared_plan_id,
            previous.identity.prepared_plan_id
        );
        assert_eq!(current.sample_rate, previous.sample_rate);
        assert_eq!(
            current.public_output_lookahead_frames,
            previous.public_output_lookahead_frames
        );
        assert_eq!(
            current.state, previous.state,
            "no entry or Session cursor advance at handoff"
        );
    }
    for previous in &old.processors {
        let current = next
            .processors
            .iter()
            .find(|current| {
                current.identity.session_id == previous.identity.session_id
                    && current.occurrence == previous.occurrence
            })
            .expect("same actual processor occurrence survives publication");
        assert_ne!(
            current.identity.prepared_plan_id,
            previous.identity.prepared_plan_id
        );
        assert_eq!(
            current.state, previous.state,
            "no DSP reset or input-prefix advance at handoff"
        );
    }
}

fn scope_repeat_assert_processor_heads(fixture: &PreflightFixture, next_sample: i64) {
    let cursors = preflight_cursors(&fixture.inner.runtime);
    assert_eq!(cursors.processors.len(), 2);
    for cursor in &cursors.processors {
        match cursor.state {
            AudioProcessorCursorState::Active { epoch, next_input_signal_sample } => {
                assert_eq!(Some(epoch.get()), fixture.inner.target.continuity_epoch);
                assert_eq!(next_input_signal_sample.sample(), next_sample);
            }
            other => panic!("Scope occurrence has no actual continued input head: {other:?}"),
        }
    }
}

#[test]
fn native_author_scope_repeated_preserved_prepare_retains_both_occurrences() {
    let spec = preflight_spec(PreflightPlacement::ScopeTwoContributions);
    let mut fixture = preflight_build(&spec, usize::MAX);
    let original_grant = fixture.inner.runtime.resource_grant();
    assert_eq!(original_grant.max_prepared_logical_bytes, usize::MAX);
    // Preserve the existing fixture's independent fixed and occurrence ceilings.
    assert_eq!(original_grant.max_fixed_resident_bytes, 64 * 1024 * 1024);
    assert_eq!(original_grant.max_runtime_occurrences, 3);
    let original_root = fixture.inner.runtime.resource_root_identity();
    let initial_footprint = fixture.inner.runtime.resource_footprint();
    assert_eq!(initial_footprint.runtime_occurrences, 1);
    assert_eq!(fixture.registry.calls.creates.load(Ordering::Relaxed), 2);
    let initial_summary = fixture
        .inner
        .runtime
        .find_resource_session(original_root)
        .unwrap()
        .resource_plan()
        .schedule_summary();
    assert_eq!(
        initial_summary.processor_occurrence_count, 2,
        "one canonical Scope is materialized into two independent owned processors"
    );
    let original_objects: Vec<_> = fixture
        .registry
        .probes
        .lock()
        .unwrap()
        .iter()
        .map(|probe| {
            let probe = probe.lock().unwrap();
            assert_eq!(probe.entries, 1);
            assert_eq!(probe.processed.as_slice(), &[(0, 0.5)]);
            (probe.entries, probe.binding.unwrap().expected_object)
        })
        .collect();
    assert_eq!(original_objects.len(), 2);
    assert_ne!(
        original_objects[0].1, original_objects[1].1,
        "the two occurrences do not alias one native-object incarnation"
    );
    scope_repeat_assert_processor_heads(&fixture, 1);
    let initial_cursors = preflight_cursors(&fixture.inner.runtime);
    let initial_claim = fixture.inner.source.current_claim().unwrap().unwrap();
    assert_eq!(
        initial_claim.generation(),
        fixture.inner.target.installed_generation
    );
    let initial_target = fixture.inner.target;

    // First operation uses A's existing public prepare/install/drop/Ack helper.
    let first = fixture.inner.value(0.75);
    let first_programs = preflight_programs(&mut fixture, 0.75);
    assert!(!first.terminal());
    assert!(first.state().is_none());
    assert!(!fixture.inner.runtime.native_author_state_entry_required(&first).unwrap());
    preflight_install(&mut fixture, &first, &first_programs);
    let first_root = fixture.inner.runtime.resource_root_identity();
    let first_claim = fixture.inner.source.current_claim().unwrap().unwrap();
    assert_eq!(first_root.session_id, original_root.session_id);
    assert_ne!(first_root.prepared_plan_id, original_root.prepared_plan_id);
    assert_eq!(
        fixture.inner.target.local_session,
        initial_target.local_session
    );
    assert_eq!(
        fixture.inner.target.continuity_epoch,
        initial_target.continuity_epoch
    );
    assert_ne!(fixture.inner.target.binding, initial_target.binding);
    assert_eq!(
        first_claim.generation(),
        fixture.inner.target.installed_generation
    );
    assert!(first_claim.generation().get() > initial_claim.generation().get());
    assert!(
        fixture.inner.probe.lock().unwrap().retained.is_none(),
        "the first exact Ack releases the real source delivery before the next proposal"
    );
    scope_repeat_assert_preserved_cursors(
        &initial_cursors,
        &preflight_cursors(&fixture.inner.runtime),
    );
    scope_repeat_assert_processor_heads(&fixture, 1);
    let first_published = fixture.inner.runtime.resource_footprint();
    let first_plan_model = fixture
        .inner
        .runtime
        .find_resource_session(first_root)
        .unwrap()
        .resource_plan()
        .session_resource_footprint()
        .unwrap();
    eprintln!(
        "SCOPE_REPEAT_FIRST_ACK: published_parameter_event_bytes={}; plan_model_parameter_event_bytes={}; logical_grant={}; fixed_grant={}; occurrence_grant={}; claim_generation={}",
        first_published.parameter_event_bytes, first_plan_model.parameter_event_bytes,
        original_grant.max_prepared_logical_bytes, original_grant.max_fixed_resident_bytes,
        original_grant.max_runtime_occurrences, first_claim.generation().get(),
    );
    // These are separate authority/model observations, neither is a borrowed
    // receipt for the currently retained Vec allocations or their capacities.
    let mut first_pcm = [93.0];
    fixture
        .inner
        .runtime
        .render_into(
            AudioRenderRequest { start_sample: 1, frames: 1 },
            &mut first_pcm,
        )
        .expect("the first installed Scope plan actually renders both occurrences");
    assert_eq!(
        first_pcm,
        [3.0],
        "two real (sample 1 + 1) * 0.75 contributions"
    );
    scope_repeat_assert_processor_heads(&fixture, 2);
    for probe in fixture.registry.probes.lock().unwrap().iter() {
        assert_eq!(probe.lock().unwrap().processed.last(), Some(&(1, 0.75)));
    }

    let second = scope_repeat_value_from_fair_cycle(&mut fixture, 0.25);
    let second_programs = preflight_programs(&mut fixture, 0.25);
    assert!(!second.terminal());
    assert!(second.state().is_none());
    assert!(
        !fixture.inner.runtime.native_author_state_entry_required(&second).unwrap(),
        "the second real prefix must also qualify for Preserved, despite two Scope occurrences"
    );
    assert_eq!(
        second.target(),
        fixture.inner.target,
        "the next delivery comes from the newly installed binding after the first Ack"
    );
    let before_second_cursors = preflight_cursors(&fixture.inner.runtime);
    let before_second_footprint = fixture.inner.runtime.resource_footprint();
    let before_second_target = fixture.inner.target;
    let before_second_claim = fixture.inner.source.current_claim().unwrap().unwrap();
    let (registration, destination) = fixture.inner.source.register().unwrap();
    let reservation = fixture
        .inner
        .source
        .reserve_install(&registration, Some(&before_second_claim))
        .unwrap();
    let mut authorization =
        fixture.inner.source.authorize_runtime_installation(reservation).unwrap();
    let cancellation = ExecutionCancellationToken::new();
    // Capture the actual typed second result before the desired-success assert.
    // This control never fabricates or forces any allocation capacity.
    let second_result = fixture.inner.runtime.prepare_native_author_parameter_handoff(
        &second_programs,
        &second,
        destination,
        &cancellation,
    );
    eprintln!(
        "SCOPE_REPEAT_SECOND_PREPARE: first_published_parameter_event_bytes={}; first_plan_model_parameter_event_bytes={}; accepted={}; error={:?}",
        first_published.parameter_event_bytes, first_plan_model.parameter_event_bytes,
        second_result.is_ok(), second_result.as_ref().err(),
    );
    assert_eq!(fixture.inner.runtime.resource_root_identity(), first_root);
    assert_eq!(
        fixture.inner.runtime.resource_footprint(),
        before_second_footprint,
        "second prepare cannot publish replacement metadata, whether accepted or refused"
    );
    let after_prepare_cursors = preflight_cursors(&fixture.inner.runtime);
    assert_eq!(
        after_prepare_cursors.sessions,
        before_second_cursors.sessions
    );
    assert_eq!(
        after_prepare_cursors.processors,
        before_second_cursors.processors
    );
    assert_eq!(
        fixture.inner.source.current_claim().unwrap().unwrap().generation(),
        before_second_claim.generation()
    );
    assert_eq!(fixture.inner.runtime.resource_grant(), original_grant);
    let mut prepared = second_result.expect(
        "second independent scalar update must prepare successfully under the unchanged generous logical grant",
    );
    let receipt = fixture
        .inner
        .runtime
        .install_native_author_parameter_handoff(&mut prepared, &mut authorization, &cancellation)
        .expect("second actual same-object Source installation");
    assert_eq!(
        receipt.continuity,
        NativeAudioParameterHandoffContinuity::Preserved
    );
    assert_eq!(
        receipt.target.root_session,
        before_second_target.root_session
    );
    assert_eq!(
        receipt.target.local_session,
        before_second_target.local_session
    );
    assert_eq!(receipt.target.instance, before_second_target.instance);
    assert_eq!(receipt.target.owner, before_second_target.owner);
    assert_eq!(receipt.target.insertion, before_second_target.insertion);
    assert_eq!(
        receipt.target.continuity_epoch,
        before_second_target.continuity_epoch
    );
    assert_ne!(receipt.target.root_plan, before_second_target.root_plan);
    assert_ne!(receipt.target.local_plan, before_second_target.local_plan);
    assert_ne!(receipt.target.binding, before_second_target.binding);
    assert_eq!(
        receipt.claim.generation(),
        receipt.target.installed_generation
    );
    assert!(receipt.claim.generation().get() > before_second_claim.generation().get());
    assert_eq!(
        fixture.inner.source.current_claim().unwrap().unwrap().generation(),
        receipt.claim.generation()
    );
    assert_eq!(
        fixture.registry.calls.creates.load(Ordering::Relaxed),
        2,
        "neither operation creates replacement processors to manufacture success"
    );
    let second_objects: Vec<_> = fixture
        .registry
        .probes
        .lock()
        .unwrap()
        .iter()
        .map(|probe| {
            let probe = probe.lock().unwrap();
            (probe.entries, probe.binding.unwrap().expected_object)
        })
        .collect();
    assert_eq!(
        second_objects, original_objects,
        "every actual object incarnation and original entry count survives both operations"
    );
    scope_repeat_assert_preserved_cursors(
        &before_second_cursors,
        &preflight_cursors(&fixture.inner.runtime),
    );
    fixture.inner.target = receipt.target;
    drop(prepared); // Caller-owned retired Plan/scratch dies after final guards.
    fixture.inner.ack(&second); // Existing exact source/current-binding Ack.
    assert!(fixture.inner.probe.lock().unwrap().retained.is_none());
    assert_eq!(
        fixture.inner.source.current_claim().unwrap().unwrap().generation(),
        receipt.claim.generation(),
        "Ack consumes the source prefix without a second Source publication"
    );
    assert_eq!(fixture.inner.runtime.resource_grant(), original_grant);
    scope_repeat_assert_processor_heads(&fixture, 2);
    let mut second_pcm = [94.0];
    fixture
        .inner
        .runtime
        .render_into(
            AudioRenderRequest { start_sample: 2, frames: 1 },
            &mut second_pcm,
        )
        .expect("second installed Scope plan continues the exact DSP prefix");
    assert_eq!(
        second_pcm,
        [1.5],
        "two real (sample 2 + 1) * 0.25 contributions"
    );
    scope_repeat_assert_processor_heads(&fixture, 3);
    for probe in fixture.registry.probes.lock().unwrap().iter() {
        let probe = probe.lock().unwrap();
        assert_eq!(
            probe.processed.as_slice(),
            &[(0, 0.5), (1, 0.75), (2, 0.25)]
        );
        assert_eq!(probe.entries, 1);
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ScopeRepeatPollProbeSnapshot {
    selected: bool,
    binding: Option<NativeAudioObjectBindRequest>,
    retained: Option<(
        NativeAudioObjectBindRequest,
        u64,
        Option<crate::NativeRequestCohort>,
    )>,
    revision: u64,
    open: u32,
    pending: Vec<NativeAudioAuthorEvent>,
    catchup_pending: Option<u32>,
    entries: usize,
    processed: Vec<(i64, f64)>,
}

fn scope_repeat_poll_probe_snapshots(
    fixture: &PreflightFixture,
) -> Vec<ScopeRepeatPollProbeSnapshot> {
    fixture
        .registry
        .probes
        .lock()
        .unwrap()
        .iter()
        .map(|owned| {
            let selected = Arc::ptr_eq(owned, &fixture.inner.probe);
            let probe = owned.lock().unwrap();
            ScopeRepeatPollProbeSnapshot {
                selected,
                binding: probe.binding,
                retained: probe.retained,
                revision: probe.revision,
                open: probe.open,
                pending: probe.pending.clone(),
                catchup_pending: probe.catchup_pending,
                entries: probe.entries,
                processed: probe.processed.clone(),
            }
        })
        .collect()
}

/// This finite fixture consumer respects the existing one-occurrence fair poll.
/// It does not change production polling, reorder a producer, or reset a cursor.
fn scope_repeat_value_from_fair_cycle(
    fixture: &mut PreflightFixture,
    value: f64,
) -> mondrian_core::NativeAudioAuthorProposal {
    let root = fixture.inner.runtime.resource_root_identity();
    let source_count = fixture
        .inner
        .runtime
        .find_resource_session(root)
        .unwrap()
        .native_observation_sources();
    assert_eq!(
        source_count, 2,
        "actual Root Session has two supported observation sources"
    );
    let old_footprint = fixture.inner.runtime.resource_footprint();
    let old_grant = fixture.inner.runtime.resource_grant();
    let old_cursors = preflight_cursors(&fixture.inner.runtime);
    let old_claim = fixture.inner.source.current_claim().unwrap().unwrap();
    let before_queue = scope_repeat_poll_probe_snapshots(fixture);
    assert_eq!(before_queue.len(), source_count);
    assert_eq!(
        before_queue.iter().filter(|probe| probe.selected).count(),
        1
    );
    for probe in &before_queue {
        assert!(probe.pending.is_empty());
        assert!(
            probe.retained.is_none(),
            "first exact Ack already released the source prefix"
        );
        assert!(
            probe.catchup_pending.is_none(),
            "this fixture has no deferred DSP feedback"
        );
    }
    let selected_before = before_queue.iter().find(|probe| probe.selected).unwrap();
    assert_eq!(
        selected_before.binding.unwrap().binding,
        fixture.inner.target.binding
    );
    assert_eq!(
        selected_before.open, 1,
        "the same nonterminal edit remains open after Ack"
    );
    fixture.inner.queue_value(value); // Queue at the original actual selected producer.
    let queued = scope_repeat_poll_probe_snapshots(fixture);
    let selected_queued = queued.iter().find(|probe| probe.selected).unwrap();
    assert_eq!(selected_queued.binding, selected_before.binding);
    assert_eq!(selected_queued.open, selected_before.open);
    assert!(selected_queued.revision > selected_before.revision);
    assert!(
        matches!(selected_queued.pending.as_slice(), [NativeAudioAuthorEvent::Parameter(event)]
        if event.parameter() == &ParameterId::new_static("native.gain")
            && event.phase() == NativeAudioAuthorParameterPhase::Value
            && event.value().map(NativeAudioAuthorScalar::bits) == Some(value.to_bits()))
    );
    for peer in queued.iter().filter(|probe| !probe.selected) {
        let old = before_queue.iter().find(|old| old.binding == peer.binding).unwrap();
        assert_eq!(
            peer, old,
            "queueing the selected callback cannot invent peer callbacks"
        );
    }
    eprintln!("SCOPE_REPEAT_FAIR_QUEUE: actual_root_sources={source_count}; probes={queued:?}");
    let cancellation = ExecutionCancellationToken::new();
    // Stable actual source count bounds exactly one round-robin cycle. Empty
    // completions are observations of one occurrence, not source-closed facts.
    for attempt in 0..source_count {
        let calls_before = fixture.registry.calls.snapshot();
        let result = fixture.inner.runtime.poll_native_observations(&cancellation);
        let probes = scope_repeat_poll_probe_snapshots(fixture);
        let calls_after = fixture.registry.calls.snapshot();
        eprintln!(
            "SCOPE_REPEAT_FAIR_POLL: attempt={}; bound={source_count}; result={:?}; calls_before={calls_before:?}; calls_after={calls_after:?}; probes={probes:?}",
            attempt + 1,
            result.as_ref().map(|batch| batch.as_ref().map(|batch|
                (batch.author.is_some(), batch.diagnostics.is_some(), batch.parameters.is_some(), batch.attachment.is_some()))),
        );
        assert_eq!(
            calls_after,
            [
                calls_before[0].checked_add(1).unwrap(),
                calls_before[1],
                calls_before[2]
            ],
            "one fair public poll attempts exactly one actual producer, with no bind/create"
        );
        assert_eq!(fixture.inner.runtime.resource_root_identity(), root);
        assert_eq!(fixture.inner.runtime.resource_footprint(), old_footprint);
        assert_eq!(fixture.inner.runtime.resource_grant(), old_grant);
        assert_eq!(
            fixture.inner.source.current_claim().unwrap().unwrap().generation(),
            old_claim.generation()
        );
        let now = preflight_cursors(&fixture.inner.runtime);
        assert_eq!(now.sessions, old_cursors.sessions);
        assert_eq!(now.processors, old_cursors.processors);
        let Some(mut batch) =
            result.expect("actual producer/control error cannot be swallowed by fair polling")
        else {
            assert_eq!(
                probes, queued,
                "a legal empty peer completion leaves the actual selected source queued"
            );
            continue;
        };
        assert!(
            batch.diagnostics.is_none(),
            "this fixture emits no diagnostic records"
        );
        assert!(
            batch.parameters.is_none(),
            "this fixture emits only the exact author callback prefix"
        );
        if let Some(attachment) = batch.attachment {
            assert_eq!(attachment.current, fixture.inner.target);
            assert!(attachment.retained);
            let object = selected_queued.binding.unwrap().expected_object;
            assert_eq!(attachment.process_id, object.process_id());
            assert_eq!(attachment.object_serial, object.object_serial());
        }
        let Some(proposal) = batch.author.take() else {
            assert_eq!(
                probes, queued,
                "non-author metadata is not permission to erase queued callbacks"
            );
            continue;
        };
        assert_eq!(proposal.target(), fixture.inner.target);
        assert_eq!(proposal.sequence(), fixture.inner.sequence.id);
        assert_eq!(proposal.revision(), selected_queued.revision);
        assert_eq!(
            proposal.callbacks(),
            &[mondrian_core::NativeAudioAuthorCallback::Parameter {
                parameter: ParameterId::new_static("native.gain"),
                phase: mondrian_core::NativeAudioParameterCallbackPhase::Value,
                value_bits: Some(value.to_bits()),
            }]
        );
        assert!(!proposal.terminal());
        assert!(proposal.state().is_none());
        assert_eq!(proposal.open_parameter_edits(), 1);
        assert!(!fixture.inner.runtime.native_author_state_entry_required(&proposal).unwrap());
        let selected = probes.iter().find(|probe| probe.selected).unwrap();
        assert!(selected.pending.is_empty());
        assert_eq!(selected.retained, Some((selected_queued.binding.unwrap(), selected_queued.revision, None)),
            "only the real selected producer transfers its exact queued prefix into retained delivery");
        assert_eq!(selected.binding, selected_queued.binding);
        assert_eq!(selected.revision, selected_queued.revision);
        assert_eq!(selected.open, selected_queued.open);
        assert_eq!(selected.entries, selected_queued.entries);
        assert_eq!(selected.processed, selected_queued.processed);
        assert_eq!(selected.catchup_pending, selected_queued.catchup_pending);
        for peer in probes.iter().filter(|probe| !probe.selected) {
            let prior = queued.iter().find(|prior| prior.binding == peer.binding).unwrap();
            assert_eq!(
                peer, prior,
                "unselected peer author data and DSP state remain untouched"
            );
        }
        return proposal;
    }
    panic!("actual selected callback was not delivered within one finite fair cycle of {source_count} sources");
}

#[test]
fn native_author_state_entry_prefix_cohort_drift_refuses_before_any_native_preparation() {
    let a = crate::NativeRequestCohort::new(
        41,
        mondrian_core::NativeAudioReprepareRequest::ClapRestart,
    )
    .unwrap();
    let b = crate::NativeRequestCohort::new(42, a.request).unwrap();
    let spec = preflight_spec(PreflightPlacement::ScopeTwoContributions);
    let mut fixture = preflight_build(&spec, usize::MAX);
    fixture.inner.probe.lock().unwrap().request_cohort = Some(a);
    fixture.inner.queue_value(0.75);
    {
        let mut probe = fixture.inner.probe.lock().unwrap();
        probe.open = 0;
        probe.revision += 1;
        probe.pending.push(NativeAudioAuthorEvent::Parameter(
            NativeAudioAuthorParameterEvent::admit(
                ParameterId::new_static("native.gain"),
                NativeAudioAuthorParameterPhase::End,
                None,
            )
            .unwrap(),
        ));
        probe.configuration = Some(0.75_f64.to_le_bytes().to_vec());
    }
    let proposal = fixture.inner.drain();
    assert!(
        fixture.inner.runtime.native_author_state_entry_required(&proposal).unwrap(),
        "coherent Scope state must reach the existing whole-tree state-entry branch"
    );
    let scope_id = match fixture.inner.target.owner {
        mondrian_core::NativeAudioEditorOccurrenceOwner::Contribution { scope_id, .. } => scope_id,
        other => panic!("expected actual Scope owner: {other:?}"),
    };
    let scope = fixture
        .inner
        .sequence
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .unwrap();
    scope.processors.processors[0].opaque_state = proposal.state().cloned();
    let programs = preflight_programs(&mut fixture, 0.75);
    let root = fixture.inner.runtime.resource_root_identity();
    let owners = scope_repeat_poll_probe_snapshots(&fixture);
    let counts = fixture.registry.calls.snapshot();
    for reported in [None, Some(b)] {
        fixture.inner.probe.lock().unwrap().reported_request_cohort = Some(reported);
        let (_, destination) = fixture.inner.source.register().unwrap();
        let error = match fixture.inner.runtime.prepare_native_author_parameter_handoff(
            &programs,
            &proposal,
            destination,
            &ExecutionCancellationToken::new(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("state-entry prefix identity drift must be refused"),
        };
        assert!(
            matches!(
                &error,
                NativeAudioParameterHandoffError::Admission(
                    NativeAudioAuthorAdmissionError::Pending {
                        reason: NativeAudioAuthorHoldReason::OutstandingDelivery,
                        ..
                    }
                )
            ),
            "actual={error:?}"
        );
        assert_eq!(fixture.inner.runtime.resource_root_identity(), root);
        assert_eq!(
            scope_repeat_poll_probe_snapshots(&fixture),
            owners,
            "no retained prefix, binding, native object or DSP history may move"
        );
        let after = fixture.registry.calls.snapshot();
        assert_eq!(
            &after[1..],
            &counts[1..],
            "no Bind or factory/create before refusal"
        );
    }
    fixture.inner.probe.lock().unwrap().reported_request_cohort = None;
    assert!(fixture.inner.drain().same_retained_proposal(&proposal));
}
