use super::*;

fn empty_partial() -> PreviewStartupShutdownEvidence {
    let (_, watch) = super::super::preview_work_notification::preview_work_notification_channel();
    PreviewStartupShutdownEvidence {
        schema_version: 1,
        opaque_panic_payload_abandoned: false,
        inventory: PreviewStartupInventory::new(2, false),
        workers: PreviewStartupWorkerShutdown {
            media: Vec::new(),
            visual: PreviewOwnedWorkerShutdown::NotStarted,
            cpu_fallback: PreviewOwnedWorkerShutdown::NotStarted,
            title: PreviewOwnedWorkerShutdown::NotStarted,
        },
        runtime: PreviewRuntimeShutdownEvidence {
            schema_version: 4,
            visual_dependency_worker: Some(PreviewOwnedWorkerShutdown::NotStarted),
            work_callbacks: Some(watch.shutdown_and_wait()),
            timeline_render_cache:
                super::super::preview_render_cache::PreviewTimelineRenderCacheShutdownEvidence {
                    schema_version: 1,
                    required: false,
                    start_failed: false,
                    worker: None,
                    aggregate_outcome: PreviewOwnedWorkerShutdown::NotStarted,
                },
            ..PreviewRuntimeShutdownEvidence::default()
        },
    }
}

#[test]
fn partial_preview_inventory_never_weakens_normal_runtime_admission() {
    let receipt = empty_partial();
    assert!(receipt.all_created_resources_released());
    assert!(!receipt.runtime.all_workers_terminated());
    let json = serde_json::to_value(&receipt).expect("raw partial receipt");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["runtime"]["schema_version"], 4);
    assert_eq!(json["inventory"]["cache"], "disabled");
    assert_eq!(json["runtime"]["visual_dependency_worker"], "not_started");
}

#[test]
fn partial_preview_rejects_unknown_missing_and_contradictory_inventory() {
    use PreviewStartupOwnerState::{Disabled, InProgress, Installed};
    let base = empty_partial();
    for index in 0..11 {
        let mut receipt = base.clone();
        match index {
            0 => receipt.inventory.visual = InProgress,
            1 => receipt.inventory.cpu_fallback = InProgress,
            2 => receipt.inventory.cache = InProgress,
            3 => receipt.inventory.observer = InProgress,
            4 => receipt.inventory.media[0] = InProgress,
            5 => receipt.inventory.visual = Installed,
            6 => receipt.inventory.media[0] = Installed,
            7 => receipt.inventory.media[0] = Disabled,
            8 => receipt.runtime.workers_started = 1,
            9 => receipt.runtime.work_callbacks = None,
            10 => receipt.opaque_panic_payload_abandoned = true,
            _ => unreachable!(),
        }
        assert!(
            !receipt.all_created_resources_released(),
            "accepted corruption {index}"
        );
    }
}

#[test]
fn partial_preview_keeps_disabled_unattempted_and_failed_cache_distinct() {
    for (state, failed) in [
        (PreviewStartupOwnerState::NotAttempted, false),
        (PreviewStartupOwnerState::Failed, true),
    ] {
        let mut receipt = empty_partial();
        receipt.inventory.cache = state;
        receipt.runtime.timeline_render_cache.required = true;
        receipt.runtime.timeline_render_cache.start_failed = failed;
        assert!(receipt.all_created_resources_released());
        receipt.runtime.timeline_render_cache.start_failed = !failed;
        assert!(!receipt.all_created_resources_released());
    }
}
