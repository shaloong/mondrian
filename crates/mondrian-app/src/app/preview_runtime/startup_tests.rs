use super::*;

fn fail_after(checkpoint: PreviewStartupCheckpoint) -> PreviewStartupFailure<()> {
    match PreviewProductionRuntime::try_start_with_checkpoint_for_test(2, |actual| {
        if actual == checkpoint {
            panic!("injected later construction failure");
        }
    }) {
        Ok(runtime) => {
            let _ = runtime.shutdown_and_wait();
            panic!("startup checkpoint not reached");
        }
        Err(failure) => failure,
    }
}

#[test]
fn preview_startup_each_real_owner_is_retained_after_later_unwind() {
    for checkpoint in [
        PreviewStartupCheckpoint::Cache,
        PreviewStartupCheckpoint::Visual,
        PreviewStartupCheckpoint::CpuFallback,
        PreviewStartupCheckpoint::Media(0),
        PreviewStartupCheckpoint::Media(1),
        PreviewStartupCheckpoint::Observer,
    ] {
        let (diagnostic, owner) = fail_after(checkpoint).into_parts();
        let before = owner.inventory.clone();
        let receipt = owner.shutdown_until(Instant::now() + Duration::from_secs(5));
        assert!(diagnostic.to_string().contains("injected later construction failure"));
        assert_eq!(receipt.inventory, before);
        assert!(
            receipt.all_created_resources_released(),
            "{checkpoint:?}: {receipt:?}"
        );
        assert_eq!(receipt.inventory.media.len(), 2);
        let expected_media = match checkpoint {
            PreviewStartupCheckpoint::Media(0) => 1,
            PreviewStartupCheckpoint::Media(1) | PreviewStartupCheckpoint::Observer => 2,
            _ => 0,
        };
        assert_eq!(receipt.workers.media.len(), expected_media);
        if checkpoint != PreviewStartupCheckpoint::Observer {
            assert!(
                !receipt.runtime.all_workers_terminated(),
                "partial is not normal Runtime"
            );
        }
    }
}

#[test]
fn preview_startup_preparation_starts_no_native_owner() {
    let runtime = PreviewProductionRuntime::<()>::prepare_unpublished(
        preview_decode_cpu_budget(),
        MediaPreviewScheduler::default(),
    );
    assert!(runtime.workers.borrow().is_empty());
    assert!(runtime.visual_execution.is_none());
    assert!(runtime.cpu_fallback_task.is_none());
    assert!(!runtime.visual_dependencies.is_healthy());
    assert!(!runtime.timeline_render_cache.borrow().worker_started());
    let owner = PreviewStartupOwner {
        runtime: Box::new(runtime),
        inventory: PreviewStartupInventory::new(2, false),
        opaque_panic_payload_abandoned: false,
    };
    let receipt = owner.shutdown_until(Instant::now() + Duration::from_secs(5));
    assert_eq!(receipt.runtime.workers_started, 0);
    assert!(receipt.all_created_resources_released());
}

#[test]
fn preview_startup_real_required_cache_and_ordinary_failure_keep_exact_inventory() {
    for fail_config in [false, true] {
        let temp = tempfile::tempdir().expect("cache root");
        let config = if fail_config {
            Err("injected cache configuration failure".to_owned())
        } else {
            Ok(mondrian_render_cache::TimelineRenderCacheConfig::new(
                temp.path().to_path_buf(),
                1_048_576,
                1_048_576,
                2,
            )
            .expect("config"))
        };
        let mut runtime = PreviewProductionRuntime::<()>::prepare_unpublished(
            preview_decode_cpu_budget(),
            MediaPreviewScheduler::default(),
        );
        *runtime.timeline_render_cache.get_mut() = crate::app::preview_render_cache::PreviewTimelineRenderCache::prepare_with_config_for_test(config);
        let failure = match PreviewProductionRuntime::try_start_prepared(
            runtime,
            0,
            PreviewWorkerIsolation::DirectTestAdapter,
            |checkpoint| {
                if checkpoint == PreviewStartupCheckpoint::Cache {
                    panic!("post-cache failure");
                }
            },
        ) {
            Err(failure) => failure,
            Ok(runtime) => {
                let _ = runtime.shutdown_and_wait();
                panic!("missing checkpoint");
            }
        };
        let (_, owner) = failure.into_parts();
        let receipt = owner.shutdown_until(Instant::now() + Duration::from_secs(5));
        assert_eq!(
            receipt.inventory.cache,
            if fail_config {
                PreviewStartupOwnerState::Failed
            } else {
                PreviewStartupOwnerState::Installed
            }
        );
        assert!(receipt.all_created_resources_released(), "{receipt:?}");
        assert_eq!(
            receipt.runtime.timeline_render_cache.worker.is_some(),
            !fail_config
        );
    }
}

#[test]
fn preview_startup_original_deadline_stays_failed_after_late_worker_return() {
    let (_, mut owner) = fail_after(PreviewStartupCheckpoint::Cache).into_parts();
    let (release, released) = mpsc::channel();
    let (finished, finish) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        // Bounded fallback also releases the fixture if an assertion unwinds.
        let _ = released.recv_timeout(Duration::from_secs(5));
        let _ = finished.send(());
    });
    owner.runtime.workers.get_mut().push(worker);
    owner.inventory.media[0] = PreviewStartupOwnerState::Installed;
    let receipt = owner.shutdown_until(Instant::now());
    let _ = release.send(());
    finish.recv_timeout(Duration::from_secs(5)).expect("late fixture return");
    assert_eq!(receipt.runtime.worker_timeouts, 1);
    assert_eq!(receipt.runtime.worker_deadline_detachments, 1);
    assert!(!receipt.all_created_resources_released());
}

#[test]
fn preview_startup_opaque_panic_is_retained_without_running_its_destructor() {
    struct HostilePayload;
    impl Drop for HostilePayload {
        fn drop(&mut self) {
            panic!("opaque destructor must not run");
        }
    }
    let failure =
        match PreviewProductionRuntime::<()>::try_start_with_checkpoint_for_test(0, |stage| {
            if stage == PreviewStartupCheckpoint::Visual {
                std::panic::panic_any(HostilePayload);
            }
        }) {
            Err(failure) => failure,
            Ok(runtime) => {
                let _ = runtime.shutdown_and_wait();
                panic!("missing injection");
            }
        };
    let (diagnostic, owner) = failure.into_parts();
    let receipt = owner.shutdown_until(Instant::now() + Duration::from_secs(5));
    assert!(crate::app::execution_panic_diagnostic::opaque_panic_payload_abandoned(&diagnostic));
    assert!(!receipt.all_created_resources_released());
}
