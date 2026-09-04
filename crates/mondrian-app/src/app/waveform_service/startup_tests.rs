use super::*;

fn failed(
    result: Result<Arc<AudioWaveformService>, AudioWaveformStartupFailure>,
) -> AudioWaveformStartupFailure {
    match result {
        Err(failure) => failure,
        Ok(_) => panic!("injected startup failure did not execute"),
    }
}

fn fail_at(stage: AudioWaveformStartupStage) -> AudioWaveformStartupFailure {
    match AudioWaveformService::try_start_with(
        |observed, _| {
            if observed == stage {
                panic!("injected Waveform startup checkpoint");
            }
        },
        spawn_analysis,
    ) {
        Err(failure) => failure,
        Ok(_) => panic!("checkpoint did not execute"),
    }
}

#[test]
fn every_startup_stage_retains_exact_real_inventory() {
    for stage in [
        AudioWaveformStartupStage::Prepared,
        AudioWaveformStartupStage::SourceCache,
        AudioWaveformStartupStage::AnalysisWorker,
    ] {
        let failure = fail_at(stage);
        assert_eq!(
            failure.diagnostic().detail,
            "injected Waveform startup checkpoint"
        );
        let receipt = failure.shutdown_until(Instant::now() + Duration::from_secs(2));
        assert_eq!(receipt.stage, stage);
        assert!(receipt.all_created_resources_released(), "{receipt:?}");
        assert_eq!(
            receipt.owner.all_resources_released(),
            stage == AudioWaveformStartupStage::AnalysisWorker
        );
        if stage != AudioWaveformStartupStage::Prepared {
            assert_eq!(
                receipt.owner.source_cache.decoder_shutdown_workers_started,
                1
            );
            assert_eq!(
                receipt.owner.source_cache.decoder_shutdown_workers_terminated,
                1
            );
            assert_eq!(
                receipt.owner.source_cache.shutdown_coordinators_terminated,
                1
            );
        }
        // Neither missing source inventory nor a different stage can qualify.
        let contradictory_stage = if stage == AudioWaveformStartupStage::Prepared {
            AudioWaveformStartupStage::SourceCache
        } else {
            AudioWaveformStartupStage::Prepared
        };
        assert!(
            !AudioWaveformStartupShutdownEvidence { stage: contradictory_stage, ..receipt }
                .all_created_resources_released()
        );
    }
}

struct HostilePanic(Arc<AtomicBool>);

impl Drop for HostilePanic {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
        panic!("opaque panic payload must not be dropped");
    }
}

#[test]
fn opaque_startup_payload_is_abandoned_without_losing_real_owners() {
    let dropped = Arc::new(AtomicBool::new(false));
    let failure = failed(AudioWaveformService::try_start_with(
        |stage, _| {
            if stage == AudioWaveformStartupStage::AnalysisWorker {
                std::panic::panic_any(HostilePanic(Arc::clone(&dropped)));
            }
        },
        spawn_analysis,
    ));
    assert!(failure.diagnostic().opaque_payload_abandoned);
    let receipt = failure.shutdown_until(Instant::now() + Duration::from_secs(2));
    assert!(receipt.owner.all_resources_released(), "{receipt:?}");
    assert!(!receipt.all_created_resources_released());
    assert!(!dropped.load(Ordering::Acquire));
}

#[test]
fn thread_start_failure_preserves_degraded_policy_and_real_source_cleanup() {
    let service = AudioWaveformService::try_start_with(
        |_, _| {},
        |work| {
            drop(work);
            Err(std::io::ErrorKind::WouldBlock.into())
        },
    )
    .expect("ordinary OS failure remains a degraded service");
    let receipt = service.shutdown_until(Instant::now() + Duration::from_secs(2));
    assert_eq!(receipt.workers_started, 0);
    assert_eq!(receipt.worker_start_failures, 1);
    assert!(receipt.source_cache.all_resources_released(), "{receipt:?}");
    assert!(!receipt.all_resources_released());
}

#[test]
fn spawner_unwind_retains_installed_source_without_inventing_analysis_worker() {
    let failure = failed(AudioWaveformService::try_start_with(
        |_, _| {},
        |work| {
            drop(work);
            panic!("analysis spawn unwind");
        },
    ));
    let receipt = failure.shutdown_until(Instant::now() + Duration::from_secs(2));
    assert_eq!(receipt.stage, AudioWaveformStartupStage::SourceCache);
    assert_eq!(receipt.owner.workers_started, 0);
    assert!(receipt.all_created_resources_released(), "{receipt:?}");
}

#[test]
fn delayed_started_worker_cannot_upgrade_original_shutdown_receipt() {
    let (release, wait) = mpsc::channel::<()>();
    let mut retained = None;
    let failure = failed(AudioWaveformService::try_start_with(
        |stage, service| {
            if stage == AudioWaveformStartupStage::AnalysisWorker {
                retained = Some(Arc::clone(service));
                panic!("after real analysis handle install");
            }
        },
        |work| {
            Ok(std::thread::spawn(move || {
                // Sender Drop also releases this worker if the test unwinds.
                let _ = wait.recv();
                work();
            }))
        },
    ));
    let service = retained.expect("actual failed owner retained for repeat observation");
    let receipt = failure.shutdown_until(Instant::now());
    assert_eq!(receipt.owner.worker_timeouts, 1);
    assert_eq!(receipt.owner.worker_detachments, 1);
    assert!(!receipt.all_created_resources_released());
    drop(release);
    let limit = Instant::now() + Duration::from_secs(2);
    while service.worker_terminal.load(Ordering::Acquire) == WAVEFORM_WORKER_TERMINAL_RUNNING {
        assert!(Instant::now() < limit, "real worker failed to return");
        std::thread::yield_now();
    }
    assert_eq!(service.shutdown_until(limit), receipt.owner);
}

#[test]
fn normal_owning_start_retains_existing_complete_shutdown_contract() {
    let service = AudioWaveformService::try_start().expect("real startup");
    let receipt = service.shutdown_until(Instant::now() + Duration::from_secs(2));
    assert!(receipt.all_resources_released(), "{receipt:?}");
}

#[test]
fn ordinary_drop_does_not_destroy_an_opaque_worker_join_payload() {
    let dropped = Arc::new(AtomicBool::new(false));
    let payload_flag = Arc::clone(&dropped);
    let service = AudioWaveformService::try_start_with(
        |_, _| {},
        |work| {
            drop(work);
            Ok(std::thread::spawn(move || {
                std::panic::panic_any(HostilePanic(payload_flag))
            }))
        },
    )
    .expect("real returned handle");
    let limit = Instant::now() + Duration::from_secs(2);
    while !service.shutdown.lock().worker.as_ref().expect("worker").is_finished() {
        assert!(Instant::now() < limit, "worker did not finish");
        std::thread::yield_now();
    }
    drop(service);
    assert!(!dropped.load(Ordering::Acquire));
}

#[test]
#[ignore = "requires a real local GPU"]
fn endurance_startup_preserves_partial_waveform_and_original_failure() {
    use crate::app::endurance_campaign::EnduranceExecutionOwners;
    use crate::app::headless_realtime_playback::HeadlessRealtimePlaybackSession;
    use crate::app::AppState;

    for stage in [
        AudioWaveformStartupStage::SourceCache,
        AudioWaveformStartupStage::AnalysisWorker,
    ] {
        let app = AppState::new();
        let failure = match EnduranceExecutionOwners::start_with_factories(
            &app,
            HeadlessRealtimePlaybackSession::new,
            || Err(fail_at(stage)),
        ) {
            Err(failure) => failure,
            Ok(_) => panic!("expected actual Waveform startup failure"),
        };
        assert!(failure
            .diagnostic()
            .to_string()
            .contains("injected Waveform startup checkpoint"));
        let (diagnostic, receipt) =
            failure.shutdown_until(app, Instant::now() + Duration::from_secs(10));
        assert!(diagnostic.to_string().contains("injected Waveform startup checkpoint"));
        assert!(!receipt.waveform_construction_unverified);
        assert!(receipt.waveform.is_none());
        assert_eq!(
            receipt.waveform_startup.expect("raw partial owner").stage,
            stage
        );
        assert!(receipt.headless.preview.is_some());
        assert!(receipt.all_created_resources_released(), "{receipt:?}");
    }
}
