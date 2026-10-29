//! Original Media producer/completion/retirement owners with the existing
//! portable adapter. This does not qualify an SDK, plugin or Source closure.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    StaleInflight,
    ReplacedRenderer,
    Current,
}

fn completion_owner_retirement(admission: Admission) {
    let (mut playback, _output, renderer, target) = setup(false);
    let retirement_thread = playback.render_retirement_worker.as_ref().unwrap().thread().id();
    let caller_thread = thread::current().id();
    let request_drops = Arc::clone(&renderer.request_drops);
    let request = renderer.request(target, 19, false);
    let proposal = request.proposal().clone();
    renderer.state_entry.store(true, Ordering::Release);
    renderer.pending.store(true, Ordering::Release);
    assert!(playback.request_native_author_handoff(request).unwrap());
    let identity = playback.native_author_handoff_in_flight.unwrap();
    // Receive the real producer completion without letting the normal poller
    // accept it. State-entry policy returns the same owned Work, before any CAS.
    let completion = match playback.completion_rx.recv_timeout(Duration::from_secs(2)).unwrap() {
        RenderCompletion::NativeAuthorHandoff(completion) => *completion,
        unexpected => {
            retire_render_completion(
                unexpected,
                &playback.render_retirement_queue,
                &playback.foreign_owner_retirements,
            );
            panic!("expected the original author handoff completion");
        }
    };
    assert_eq!(renderer.policy_queries.load(Ordering::Acquire), 1);
    assert_eq!(renderer.installs.load(Ordering::Acquire), 0);
    assert_eq!(renderer.acks.load(Ordering::Acquire), 0);
    assert!(
        request_drops.lock().is_empty(),
        "completion still owns the original request"
    );

    let target_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let expected_inflight = match admission {
            Admission::StaleInflight => {
                // Model a different currently tracked admission without moving
                // or recreating the actual old completion/request owner.
                let different = (identity.0.checked_add(1).unwrap(), identity.1);
                playback.native_author_handoff_in_flight = Some(different);
                Some(different)
            }
            Admission::ReplacedRenderer => {
                let replacement = Renderer::new(false);
                playback
                    .prepare(
                        sample_position(0),
                        replacement,
                        AudioPcmContinuityModel::GenerationState,
                    )
                    .unwrap();
                assert_ne!(
                    playback.renderer.as_ref().unwrap().materialization_id,
                    identity.1
                );
                assert_eq!(playback.native_author_handoff_in_flight, Some(identity),
                    "this completion reaches the renderer-stale branch after matching its in-flight identity");
                None
            }
            Admission::Current => None,
        };
        let mut events = Vec::new();
        playback.accept_native_author_handoff_completion(completion, &mut events);
        assert!(
            events.is_empty(),
            "a waiting or stale completion publishes no successful handoff"
        );
        assert_eq!(playback.native_author_handoff_in_flight, expected_inflight);
        if admission == Admission::Current {
            let retained = playback
                .native_author_state_entry_request
                .as_ref()
                .expect("current completion retains the same request for output retirement");
            assert!(retained.request.proposal().same_retained_proposal(&proposal));
            assert_eq!(retained.request.expected_target(), target);
            assert!(
                request_drops.lock().is_empty(),
                "current Work is not retired early"
            );
        } else {
            assert!(playback.native_author_state_entry_request.is_none());
            let deadline = Instant::now() + Duration::from_secs(2);
            while request_drops.lock().is_empty() && Instant::now() < deadline {
                thread::yield_now();
            }
            assert_eq!(request_drops.lock().as_slice(), &[retirement_thread],
                "the request's sole owner must retire on the original worker, never the completion caller");
        }
        assert_eq!(renderer.installs.load(Ordering::Acquire), 0);
        assert_eq!(renderer.acks.load(Ordering::Acquire), 0);
    }));
    drop(renderer);
    let evidence = playback.shutdown_and_wait();
    if let Err(payload) = target_result {
        std::panic::resume_unwind(payload);
    }
    assert!(evidence.all_workers_terminated(), "{evidence:?}");
    assert_eq!(evidence.foreign_owner_panics, 0, "{evidence:?}");
    assert_eq!(evidence.foreign_owner_abandonments, 0, "{evidence:?}");
    assert_eq!(
        request_drops.lock().as_slice(),
        &[retirement_thread],
        "exactly one request retirement"
    );
    assert_ne!(retirement_thread, caller_thread);
}

#[test]
fn stale_inflight_author_completion_retires_request_on_original_worker() {
    completion_owner_retirement(Admission::StaleInflight);
}

#[test]
fn replaced_renderer_author_completion_retires_request_on_original_worker() {
    completion_owner_retirement(Admission::ReplacedRenderer);
}

#[test]
fn current_author_completion_retains_request_until_original_shutdown_retirement() {
    completion_owner_retirement(Admission::Current);
}
