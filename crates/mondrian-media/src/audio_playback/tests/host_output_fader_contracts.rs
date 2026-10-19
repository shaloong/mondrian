//! Portable Source/custody controls; the adapter below is not native DSP evidence.
use super::*;

fn installed_source() -> (
    AudioPlayback,
    Arc<Mutex<FakeOutputState>>,
    Arc<NativeInstallationProbe>,
    Arc<dyn AudioPcmRenderer>,
) {
    let (mut playback, output) = playback_with_buffered_frames(0);
    let probe = NativeInstallationProbe::new(false, false);
    let renderer: Arc<dyn AudioPcmRenderer> = probe.clone();
    playback
        .prepare(
            sample_position(0),
            renderer.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .expect("original actual Source slot");
    settle_native_installation(&mut playback);
    assert_eq!(
        playback.native_owner_binding_status(),
        Some(AudioPcmNativeOwnerBindingStatus::Ready)
    );
    (playback, output, probe, renderer)
}

#[test]
fn host_output_fader_source_publication_preserves_fifo_and_rejection_custody() {
    let (mut playback, output, probe, renderer) = installed_source();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let verifier = probe.verifier();
        let before = verifier.current_claim().unwrap();
        let expected = Arc::downgrade(&renderer);
        playback
            .output
            .enqueue(&AudioBuffer::silent(
                playback.config.sample_rate,
                AudioChannelLayout::Stereo,
                11,
            ))
            .unwrap();
        let generation = playback.generation;
        let next = playback.next_start_sample;
        let anchor = playback.media_anchor;
        let callback_calls = std::cell::Cell::new(0);
        let foreign_source = AudioOwnerInstallationSource::new();
        let (_, foreign) = foreign_source.register().unwrap();
        let foreign_result =
            playback.with_host_output_fader_publication(&expected, &foreign, |_, _| {
                callback_calls.set(callback_calls.get() + 1);
                Ok::<_, ()>(())
            });
        assert!(matches!(
            foreign_result,
            Err(AudioPlaybackError::OwnerInstallation(
                AudioOwnerInstallationError::WrongRegistration
            ))
        ));
        let other: Arc<dyn AudioPcmRenderer> = NativeInstallationProbe::new(false, false);
        let wrong_slot = playback.with_host_output_fader_publication(
            &Arc::downgrade(&other),
            &verifier,
            |_, _| {
                callback_calls.set(callback_calls.get() + 1);
                Ok::<_, ()>(())
            },
        );
        assert!(matches!(
            wrong_slot,
            Err(AudioPlaybackError::HostOutputFaderSourceChanged)
        ));
        assert_eq!(callback_calls.get(), 0);
        let rejected = playback
            .with_host_output_fader_publication(&expected, &verifier, |_, _| {
                callback_calls.set(callback_calls.get() + 1);
                Err::<(), _>("dependency refused before Source CAS")
            })
            .unwrap();
        assert_eq!(rejected, Err("dependency refused before Source CAS"));
        assert!(verifier.validate(&before).is_ok());
        assert_eq!(
            playback.native_owner_binding_status(),
            Some(AudioPcmNativeOwnerBindingStatus::Ready)
        );
        assert_eq!(output.lock().queued_frames, 11);

        let after = playback
            .with_host_output_fader_publication(
                &expected,
                &verifier,
                |destination, authorization| {
                    callback_calls.set(callback_calls.get() + 1);
                    let publication = authorization.begin_commit(destination)?;
                    Ok::<_, AudioOwnerInstallationError>(publication.commit())
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(callback_calls.get(), 2);
        assert!(verifier.validate(&before).is_err());
        assert!(verifier.validate(&after).is_ok());
        assert!(after.generation() > before.generation());
        assert!(playback.matches_source(&renderer));
        assert_eq!(playback.native_owner_binding_status(), None);
        assert_eq!(playback.generation, generation);
        assert_eq!(playback.next_start_sample, next);
        assert_eq!(playback.media_anchor, anchor);
        assert_eq!(playback.in_flight, 0);
        assert_eq!(output.lock().queued_frames, 11);
    }));
    let cleanup = playback.shutdown_until(Instant::now() + Duration::from_secs(2));
    assert!(
        cleanup.all_workers_terminated(),
        "original consuming cleanup: {cleanup:?}"
    );
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn host_output_fader_refuses_actual_unaccepted_pcm_work_before_publication() {
    let (mut playback, _, probe, renderer) = installed_source();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let verifier = probe.verifier();
        let before = verifier.current_claim().unwrap();
        let poll = playback.poll(AudioPlaybackMode::Consume, sample_position(0)).unwrap();
        assert!(
            poll.snapshot.in_flight > 0,
            "real serial PCM admission, including unaccepted completion"
        );
        let generation = playback.generation;
        let next = playback.next_start_sample;
        let result = playback.with_host_output_fader_publication(
            &Arc::downgrade(&renderer),
            &verifier,
            |_, _| -> Result<(), ()> {
                panic!("busy Source must refuse before dependency publication")
            },
        );
        assert!(matches!(
            result,
            Err(AudioPlaybackError::HostOutputFaderBusy)
        ));
        assert!(verifier.validate(&before).is_ok());
        assert_eq!(playback.generation, generation);
        assert_eq!(playback.next_start_sample, next);
        assert_eq!(
            playback.native_owner_binding_status(),
            Some(AudioPcmNativeOwnerBindingStatus::Ready)
        );
    }));
    let cleanup = playback.shutdown_until(Instant::now() + Duration::from_secs(2));
    assert!(
        cleanup.all_workers_terminated(),
        "original consuming cleanup: {cleanup:?}"
    );
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
}
