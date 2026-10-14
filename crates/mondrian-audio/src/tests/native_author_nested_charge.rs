//! New independent charge controls; requires the reviewed four-file charge candidate.
//! This owned ABI evidence does not qualify a native DLL, allocator completeness or RT.
use super::*;

fn nested_charge_sum(runtime: &AudioProgramRuntime) -> (usize, usize) {
    let cursors = preflight_cursors(runtime);
    let mut parameters = 0_usize;
    let mut fixed = 0_usize;
    for cursor in &cursors.sessions {
        let weights = runtime
            .find_resource_session(cursor.identity)
            .unwrap()
            .current_resource_footprint()
            .unwrap();
        parameters = parameters.checked_add(weights.parameter_event_bytes).unwrap();
        fixed = fixed.checked_add(weights.fixed_resident_bytes).unwrap();
    }
    (parameters, fixed)
}

fn nested_charge_assert_aggregate(runtime: &AudioProgramRuntime, window_bytes: usize) {
    let (parameters, local_fixed) = nested_charge_sum(runtime);
    let aggregate = runtime.resource_footprint();
    assert_eq!(
        aggregate.parameter_event_bytes, parameters,
        "root published parameter component is the sum of actual materialized local charges"
    );
    assert_eq!(
        aggregate.fixed_resident_bytes,
        local_fixed.checked_add(window_bytes).unwrap(),
        "same local charge adjustment applies to fixed residency, retaining source windows"
    );
}

fn nested_charge_value_from_fair_cycle(
    fixture: &mut PreflightFixture,
    value: f64,
) -> mondrian_core::NativeAudioAuthorProposal {
    let cursors = preflight_cursors(&fixture.inner.runtime);
    let root = fixture.inner.runtime.resource_root_identity();
    let footprint = fixture.inner.runtime.resource_footprint();
    let grant = fixture.inner.runtime.resource_grant();
    let claim = fixture.inner.source.current_claim().unwrap().unwrap();
    let sources = cursors
        .sessions
        .iter()
        .try_fold(0_usize, |count, cursor| {
            count.checked_add(
                fixture
                    .inner
                    .runtime
                    .find_resource_session(cursor.identity)
                    .unwrap()
                    .native_observation_sources(),
            )
        })
        .unwrap();
    assert_eq!(
        sources, 2,
        "two actual supported producers across three Sessions"
    );
    assert_eq!(
        fixture
            .inner
            .runtime
            .find_resource_session(root)
            .unwrap()
            .native_observation_sources(),
        0,
        "the Root itself has no native producer"
    );
    let old_objects: Vec<_> = fixture
        .registry
        .probes
        .lock()
        .unwrap()
        .iter()
        .map(|owned| {
            let probe = owned.lock().unwrap();
            assert!(probe.pending.is_empty());
            assert!(probe.retained.is_none());
            assert!(probe.catchup_pending.is_none());
            (
                probe.entries,
                probe.binding.unwrap().expected_object,
                probe.processed.clone(),
            )
        })
        .collect();
    fixture.inner.queue_value(value);
    for attempt in 0..sources {
        let calls = fixture.registry.calls.snapshot();
        let result = fixture
            .inner
            .runtime
            .poll_native_observations(&ExecutionCancellationToken::new());
        let after_calls = fixture.registry.calls.snapshot();
        assert_eq!(
            after_calls,
            [calls[0].checked_add(1).unwrap(), calls[1], calls[2]],
            "each fair poll visits one real producer with no Bind/create"
        );
        assert_eq!(fixture.inner.runtime.resource_root_identity(), root);
        assert_eq!(fixture.inner.runtime.resource_footprint(), footprint);
        assert_eq!(fixture.inner.runtime.resource_grant(), grant);
        assert_eq!(
            fixture.inner.source.current_claim().unwrap().unwrap().generation(),
            claim.generation()
        );
        let after = preflight_cursors(&fixture.inner.runtime);
        assert_eq!(after.sessions, cursors.sessions);
        assert_eq!(after.processors, cursors.processors);
        let objects: Vec<_> = fixture
            .registry
            .probes
            .lock()
            .unwrap()
            .iter()
            .map(|owned| {
                let probe = owned.lock().unwrap();
                (
                    probe.entries,
                    probe.binding.unwrap().expected_object,
                    probe.processed.clone(),
                )
            })
            .collect();
        assert_eq!(objects, old_objects, "poll cannot Enter or process PCM");
        eprintln!(
            "NESTED_CHARGE_FAIR_POLL: attempt={}; bound={sources}; error={:?}",
            attempt + 1,
            result.as_ref().err()
        );
        let Some(batch) = result.expect("actual producer refusal remains visible") else {
            continue;
        };
        assert!(batch.diagnostics.is_none());
        assert!(batch.parameters.is_none());
        if let Some(attachment) = batch.attachment {
            assert_eq!(attachment.current, fixture.inner.target);
        }
        let Some(proposal) = batch.author else {
            continue;
        };
        assert_eq!(proposal.target(), fixture.inner.target);
        assert!(!proposal.terminal());
        assert!(proposal.state().is_none());
        assert!(!fixture.inner.runtime.native_author_state_entry_required(&proposal).unwrap());
        return proposal;
    }
    panic!("the original selected producer must deliver within its actual whole-tree fair cycle");
}

#[test]
fn native_author_nested_repeated_preserved_charge_matches_materialized_locals() {
    let mut spec = preflight_spec(PreflightPlacement::NestedTwoPlacements);
    // New test setup only: the constructor has an explicit 0 dB unity lane.
    // Two processor lanes per child share one maximum-sized parameter scratch.
    spec.canonical.audio_program.outputs[0].strip.pre_fader.processors.push(
        AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
    );
    spec.dependencies[0] = spec.canonical.clone();
    let mut fixture = preflight_build(&spec, usize::MAX);
    let grant = fixture.inner.runtime.resource_grant();
    assert_eq!(grant.max_prepared_logical_bytes, usize::MAX);
    assert_eq!(grant.max_fixed_resident_bytes, 64 * 1024 * 1024);
    assert_eq!(grant.max_runtime_occurrences, 3);
    let initial = fixture.inner.runtime.resource_footprint();
    let initial_cursors = preflight_cursors(&fixture.inner.runtime);
    assert_eq!(initial_cursors.sessions.len(), 3);
    assert_eq!(initial_cursors.processors.len(), 4);
    assert_eq!(fixture.registry.probes.lock().unwrap().len(), 2);
    let (_, initial_local_fixed) = nested_charge_sum(&fixture.inner.runtime);
    let source_windows = initial.fixed_resident_bytes.checked_sub(initial_local_fixed).unwrap();
    assert_eq!(
        source_windows,
        initial.media_window_bytes.checked_add(initial.nested_window_bytes).unwrap()
    );
    nested_charge_assert_aggregate(&fixture.inner.runtime, source_windows);
    for cursor in &initial_cursors.sessions {
        let session = fixture.inner.runtime.find_resource_session(cursor.identity).unwrap();
        let model = session.resource_plan().session_resource_footprint().unwrap();
        assert_eq!(
            session.current_resource_footprint().unwrap(),
            model,
            "initial component uses the existing model admission basis"
        );
        if cursor.identity != fixture.inner.runtime.resource_root_identity() {
            assert_eq!(
                session.resource_plan().schedule_summary().processor_occurrence_count,
                2
            );
            assert_eq!(
                session.resource_plan().schedule_summary().processor_parameter_lane_count,
                2
            );
        }
    }
    // Approved test-only descendant evidence reads exact cached Runtime values.
    let initial_subtrees = crate::runtime::native_author::charge_test_controls::subtree_footprints(
        &fixture.inner.runtime,
    );
    assert_eq!(initial_subtrees.len(), 3);
    let initial_root = fixture.inner.runtime.resource_root_identity();
    let child_windows: Vec<_> = initial_subtrees.iter().filter(|(identity, _)| *identity != initial_root)
        .map(|(identity, cached)| {
            let local = fixture.inner.runtime.find_resource_session(*identity).unwrap()
                .current_resource_footprint().unwrap();
            assert_eq!(cached.parameter_event_bytes, local.parameter_event_bytes);
            let windows = cached.fixed_resident_bytes.checked_sub(local.fixed_resident_bytes).unwrap();
            assert_eq!(windows, cached.media_window_bytes.checked_add(cached.nested_window_bytes).unwrap(),
                "this child has no grandchildren, so its initial fixed residue is its actual source window");
            (identity.session_id, windows)
        }).collect();
    assert_eq!(child_windows.len(), 2);
    assert_ne!(child_windows[0].0, child_windows[1].0);
    for (start, value, expected_pcm) in [(1, 0.75, 3.0), (2, 0.25, 1.5), (3, 0.25, 2.0)] {
        let before = preflight_cursors(&fixture.inner.runtime);
        let selected = fixture.inner.target.local_session;
        let claim = fixture.inner.source.current_claim().unwrap().unwrap();
        let proposal = nested_charge_value_from_fair_cycle(&mut fixture, value);
        let programs = preflight_programs(&mut fixture, value);
        preflight_install(&mut fixture, &proposal, &programs);
        let installed_claim = fixture.inner.source.current_claim().unwrap().unwrap();
        assert!(installed_claim.generation().get() > claim.generation().get());
        assert_eq!(
            installed_claim.generation(),
            fixture.inner.target.installed_generation
        );
        fixture
            .inner
            .runtime
            .native_owner_installation_verifier()
            .unwrap()
            .validate(&installed_claim)
            .unwrap();
        assert_eq!(fixture.inner.runtime.resource_grant(), grant);
        nested_charge_assert_aggregate(&fixture.inner.runtime, source_windows);
        let after = preflight_cursors(&fixture.inner.runtime);
        let current_subtrees =
            crate::runtime::native_author::charge_test_controls::subtree_footprints(
                &fixture.inner.runtime,
            );
        assert_eq!(current_subtrees.len(), after.sessions.len());
        for cursor in &after.sessions {
            let matching: Vec<_> = current_subtrees
                .iter()
                .filter(|(identity, _)| *identity == cursor.identity)
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "one cached subtree for each actual materialized Session"
            );
            let (identity, cached) = matching[0];
            if *identity == fixture.inner.runtime.resource_root_identity() {
                assert_eq!(
                    *cached,
                    fixture.inner.runtime.resource_footprint(),
                    "descendant evidence includes the exact current Root cache"
                );
            } else {
                let local = fixture
                    .inner
                    .runtime
                    .find_resource_session(*identity)
                    .unwrap()
                    .current_resource_footprint()
                    .unwrap();
                let matching_windows: Vec<_> = child_windows
                    .iter()
                    .filter(|(session_id, _)| *session_id == identity.session_id)
                    .collect();
                assert_eq!(matching_windows.len(), 1);
                assert_eq!(
                    cached.parameter_event_bytes, local.parameter_event_bytes,
                    "child cached parameter component changes with its own scratch/charged owner"
                );
                assert_eq!(cached.fixed_resident_bytes,
                    local.fixed_resident_bytes.checked_add(matching_windows[0].1).unwrap(),
                    "child fixed cache changes by the same component while retaining its initial window charge");
            }
        }
        if start == 3 {
            for old in &before.sessions {
                let now = after
                    .sessions
                    .iter()
                    .find(|now| now.identity.session_id == old.identity.session_id)
                    .unwrap();
                if old.identity.session_id == selected {
                    assert_ne!(now.identity.prepared_plan_id, old.identity.prepared_plan_id);
                } else {
                    assert_eq!(
                        now.identity, old.identity,
                        "equal non-selected child and unchanged Root keep their exact Plans"
                    );
                }
            }
        }
        let mut pcm = [93.0];
        fixture
            .inner
            .runtime
            .render_into(
                AudioRenderRequest { start_sample: start, frames: 1 },
                &mut pcm,
            )
            .unwrap();
        assert_eq!(
            pcm,
            [expected_pcm],
            "two child signals and unity processors render literally"
        );
        let rendered = preflight_cursors(&fixture.inner.runtime);
        for processor in &rendered.processors {
            if processor.occurrence.instance_id == spec.instance {
                assert!(
                    matches!(processor.state, AudioProcessorCursorState::Active {
                    next_input_signal_sample, ..
                } if next_input_signal_sample.sample() == start + 1),
                    "each native processor advances exactly one actual signal frame"
                );
            } else {
                assert_eq!(
                    processor.state,
                    AudioProcessorCursorState::Stateless,
                    "Builtin Gain keeps its actual stateless execution contract"
                );
            }
        }
    }
    assert_eq!(fixture.registry.calls.creates.load(Ordering::Relaxed), 2);
    for probe in fixture.registry.probes.lock().unwrap().iter() {
        let probe = probe.lock().unwrap();
        assert_eq!(probe.entries, 1);
        assert_eq!(
            probe.processed.as_slice(),
            &[(0, 0.5), (1, 0.75), (2, 0.25), (3, 0.25)]
        );
        assert!(probe.retained.is_none());
    }
}
