use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use mondrian_core::{AssetId, ExecutionCancellationToken, ExecutionTerminalDisposition};
use mondrian_media::{info::ChannelLayout, AudioSourceSelection};

use super::analysis::resample_peaks;
use super::state::{
    duration_to_waveform_frames, push_terminal, rotate_waveform_generation, PendingWaveform,
    WaveformFailure, WaveformJob, WaveformResult, WaveformSource, WaveformSourceKey, WaveformState,
    WaveformWorkerIdentity,
};
use super::{
    AudioWaveformService, WaveformFailureReason, WaveformWorkerPhase, WAVEFORM_MAX_WIDTH,
    WAVEFORM_SOURCE_CACHE_BYTE_BUDGET, WAVEFORM_SOURCE_WINDOW_ENTRIES,
    WAVEFORM_TERMINAL_EVIDENCE_CAPACITY,
};

fn selection(revision: u64) -> AudioSourceSelection {
    AudioSourceSelection::new(
        0,
        ChannelLayout::Mono,
        mondrian_core::MediaFileFingerprint {
            len: Some(revision),
            modified_secs: Some(revision),
            modified_nanos: Some(0),
            object_identity: Some(mondrian_core::MediaFileObjectIdentity::Unix {
                device: 1,
                inode: revision,
            }),
            change_stamp: Some(mondrian_core::MediaFileChangeStamp::Unix {
                seconds: revision as i64,
                nanoseconds: 0,
            }),
        },
    )
}

fn source_key(revision: u64) -> WaveformSourceKey {
    WaveformSourceKey {
        asset_id: AssetId::new(),
        selection: selection(revision),
    }
}

#[test]
fn source_key_uses_complete_stream_and_file_revision_identity() {
    let first = source_key(42);
    let mut second = first.clone();
    second.selection = AudioSourceSelection::new(
        first.selection.stream_index(),
        first.selection.source_layout().clone(),
        mondrian_core::MediaFileFingerprint {
            object_identity: Some(mondrian_core::MediaFileObjectIdentity::Unix {
                device: 1,
                inode: 99,
            }),
            ..first.selection.source_fingerprint()
        },
    );

    assert_ne!(first, second);
}

#[test]
fn duration_projection_uses_ceil_and_rejects_zero() {
    assert_eq!(duration_to_waveform_frames(Duration::ZERO), None);
    assert_eq!(
        duration_to_waveform_frames(Duration::from_nanos(1)),
        Some(1)
    );
    assert_eq!(
        duration_to_waveform_frames(Duration::from_secs(10)),
        Some(480_000)
    );
}

#[test]
fn resampling_is_bounded_and_preserves_source_extremes() {
    let output = resample_peaks(&[0.25, 0.5, 1.0], u32::MAX);
    assert_eq!(output.len(), WAVEFORM_MAX_WIDTH as usize);
    assert_eq!(output[0], 0.25);
    assert!(output.contains(&1.0));

    let downsampled = resample_peaks(&[0.0, 0.0, 1.0, 0.0, 0.0], 2);
    assert_eq!(downsampled.len(), 2);
    assert!(downsampled.contains(&1.0));
}

#[test]
fn library_rotation_cancels_pending_and_clears_project_state() {
    let service = AudioWaveformService::new();
    let key = source_key(7);
    let cancellation = ExecutionCancellationToken::new();
    {
        let mut state = service.state.lock();
        state.pending.insert(
            key.clone(),
            PendingWaveform { generation: 1, cancellation: cancellation.clone() },
        );
        state.failures.insert(
            key,
            WaveformFailure::new(WaveformFailureReason::DecodeFailed, "test"),
        );
    }

    service.set_library(None);
    assert!(
        !cancellation.is_canceled(),
        "unchanged None binding is a no-op"
    );
    let mut state = service.state.lock();
    rotate_waveform_generation(&mut state, None);
    assert!(cancellation.is_canceled());
    assert!(state.pending.is_empty());
    assert!(state.failures.is_empty());
    assert_eq!(state.generation, 2);
}

#[test]
fn terminal_evidence_is_bounded() {
    let mut state = WaveformState::default();
    let key = source_key(1);
    for _ in 0..WAVEFORM_TERMINAL_EVIDENCE_CAPACITY + 5 {
        push_terminal(
            &mut state,
            &key,
            1,
            ExecutionTerminalDisposition::Completed,
            Duration::ZERO,
            None,
        );
    }
    assert_eq!(
        state.terminal_records.len(),
        WAVEFORM_TERMINAL_EVIDENCE_CAPACITY
    );
}

#[test]
fn unresolved_dependency_produces_headless_terminal_evidence() {
    let service = AudioWaveformService::new();
    let asset_id = AssetId::new();
    assert_eq!(
        service.source().lookup(asset_id, &selection(1), 0.0, 1.0, 100),
        None
    );

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.failures, 1);
    assert_eq!(diagnostics.retained_failures, 1);
    assert_eq!(diagnostics.terminal_records.len(), 1);
    let terminal = &diagnostics.terminal_records[0];
    assert_eq!(terminal.asset_id, asset_id);
    assert_eq!(
        terminal.evidence.disposition,
        ExecutionTerminalDisposition::Failed
    );
    assert_eq!(
        terminal.failure,
        Some(WaveformFailureReason::LibraryUnavailable)
    );
}

#[test]
fn canceled_deferred_demand_is_retired_without_worker_admission() {
    let service = AudioWaveformService::new();
    let key = source_key(9);
    let cancellation = ExecutionCancellationToken::new();
    cancellation.cancel();
    {
        let mut state = service.state.lock();
        state.pending.insert(
            key.clone(),
            PendingWaveform { generation: 1, cancellation: cancellation.clone() },
        );
        state.deferred.push_back(WaveformJob {
            key: key.clone(),
            generation: 1,
            path: PathBuf::from("unused-canceled-source.wav"),
            total_frames: 1,
            cancellation,
        });
    }

    service.dispatch_deferred(1);

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.pending_sources, 0);
    assert_eq!(diagnostics.deferred_sources, 0);
    assert_eq!(diagnostics.cancellations, 1);
    assert_eq!(
        diagnostics.terminal_records[0].evidence.disposition,
        ExecutionTerminalDisposition::Canceled
    );
}

#[test]
fn resource_policy_pauses_dispatch_trims_residency_and_recovers() {
    let service = AudioWaveformService::new();
    let key = source_key(10);
    let cancellation = ExecutionCancellationToken::new();
    {
        let mut state = service.state.lock();
        let source = WaveformSource {
            envelope: vec![0.25_f32; 8].into(),
            total_frames: 8,
            sample_rate: 48_000,
        };
        state.cached_source_bytes = source.envelope.len() * std::mem::size_of::<f32>();
        state.sources.insert(key.clone(), source);
        state.source_lru.push_front(key.clone());
        state.pending.insert(
            key.clone(),
            PendingWaveform { generation: 1, cancellation: cancellation.clone() },
        );
        state.deferred.push_back(WaveformJob {
            key,
            generation: 1,
            path: PathBuf::from("unused-resource-paused-source.wav"),
            total_frames: 1,
            cancellation,
        });
    }

    service.set_resource_policy(false, false, 1);
    service.dispatch_deferred(1);
    let paused = service.diagnostics();
    assert!(!paused.automatic_admission_enabled);
    assert!(!paused.dispatch_enabled);
    assert_eq!(paused.cached_sources, 0);
    assert_eq!(paused.deferred_sources, 1);

    service.set_resource_policy(true, true, WAVEFORM_SOURCE_CACHE_BYTE_BUDGET);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        service.poll_finished();
        let diagnostics = service.diagnostics();
        if diagnostics.pending_sources == 0 {
            assert!(diagnostics.dispatch_enabled);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "waveform worker did not terminate resumed work"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn diagnostics_distinguish_queued_running_and_awaiting_publication() {
    let service = AudioWaveformService::new();
    let key = source_key(402);
    let cancellation = ExecutionCancellationToken::new();
    service
        .state
        .lock()
        .pending
        .insert(key.clone(), PendingWaveform { generation: 1, cancellation });
    service.state.lock().active_keys.insert(key.asset_id, key.clone());
    let identity = WaveformWorkerIdentity { key: key.clone(), generation: 1 };
    let mut lease = service.worker_activity.begin(identity);

    let waiting = service.diagnostics();
    assert_eq!(
        waiting.worker_phase,
        WaveformWorkerPhase::WaitingForDispatch
    );
    assert_eq!(waiting.queued_sources, 1);
    assert_eq!(waiting.running_sources, 0);

    lease.mark_running();
    let running = service.diagnostics();
    assert_eq!(running.worker_phase, WaveformWorkerPhase::Running);
    assert_eq!(running.queued_sources, 0);
    assert_eq!(running.running_sources, 1);

    lease.finish_for_publication();
    lease.commit_publication();
    let awaiting = service.diagnostics();
    assert_eq!(awaiting.worker_phase, WaveformWorkerPhase::Idle);
    assert_eq!(awaiting.queued_sources, 0);
    assert_eq!(awaiting.running_sources, 0);
    assert_eq!(awaiting.awaiting_publication, 1);

    assert!(service.publish_result(WaveformResult {
        key,
        generation: 1,
        source: Ok(WaveformSource {
            envelope: vec![0.25_f32].into(),
            total_frames: 1,
            sample_rate: 48_000,
        }),
        elapsed: Duration::ZERO,
    }));
    assert_eq!(service.diagnostics().awaiting_publication, 0);
}

#[test]
fn resource_policy_partitions_waveform_envelopes_and_pcm_source_residency() {
    let service = AudioWaveformService::new();

    service.set_resource_policy(true, true, 16 * 1024 * 1024);
    let constrained = service.diagnostics();
    assert_eq!(constrained.aggregate_cache_byte_budget, 16 * 1024 * 1024);
    assert_eq!(constrained.source_cache_byte_budget, 12 * 1024 * 1024);
    assert_eq!(constrained.source_cache.byte_budget, 4 * 1024 * 1024);
    assert_eq!(constrained.source_cache.entry_capacity, 1);
    assert_eq!(constrained.source_cache.decoder_session_capacity, 1);

    service.set_resource_policy(true, true, 64 * 1024 * 1024);
    let expanded = service.diagnostics();
    assert_eq!(expanded.aggregate_cache_byte_budget, 64 * 1024 * 1024);
    assert_eq!(expanded.source_cache_byte_budget, 48 * 1024 * 1024);
    assert_eq!(expanded.source_cache.byte_budget, 16 * 1024 * 1024);
    assert_eq!(
        expanded.source_cache.entry_capacity,
        WAVEFORM_SOURCE_WINDOW_ENTRIES
    );
    assert_eq!(expanded.source_cache.decoder_session_capacity, 1);
}

#[test]
fn dispatch_gate_holds_transported_waveform_work_until_resume() {
    let service = AudioWaveformService::new();
    let key = source_key(11);
    let cancellation = ExecutionCancellationToken::new();
    {
        service.state.lock().pending.insert(
            key.clone(),
            PendingWaveform { generation: 1, cancellation: cancellation.clone() },
        );
    }
    service.set_resource_policy(false, false, WAVEFORM_SOURCE_CACHE_BYTE_BUDGET);
    service
        .jobs
        .lock()
        .as_ref()
        .expect("waveform sender")
        .try_send(WaveformJob {
            key,
            generation: 1,
            path: PathBuf::from("unused-gated-waveform-source.wav"),
            total_frames: 1,
            cancellation,
        })
        .expect("transport one paused waveform");

    std::thread::sleep(Duration::from_millis(20));
    assert!(matches!(
        service.results.lock().try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));

    service.set_resource_policy(true, true, WAVEFORM_SOURCE_CACHE_BYTE_BUDGET);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        service.poll_finished();
        if service.diagnostics().pending_sources == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "waveform gate did not resume transported work"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn source_adapter_is_weak_and_clean_shutdown_reclaims_every_owner() {
    let service = AudioWaveformService::new();
    let source = service.source();
    assert_eq!(Arc::strong_count(&service), 1);

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let evidence = service.shutdown_until(deadline);
    assert_eq!(evidence.schema_version, 1);
    assert_eq!(evidence.workers_started, 1);
    assert_eq!(evidence.workers_terminated, 1);
    assert!(evidence.all_resources_released(), "{evidence:#?}");
    assert_eq!(service.shutdown_until(deadline), evidence);

    let key = source_key(501);
    assert!(source.lookup(key.asset_id, &key.selection, 0.0, 1.0, 64).is_none());
    drop(service);
    assert!(source.lookup(key.asset_id, &key.selection, 0.0, 1.0, 64).is_none());
}

#[test]
fn shutdown_boundary_cancels_deferred_demand_and_closes_source_cache() {
    let service = AudioWaveformService::new();
    let key = source_key(502);
    let cancellation = ExecutionCancellationToken::new();
    {
        let mut state = service.state.lock();
        state.dispatch_enabled = false;
        state.pending.insert(
            key.clone(),
            PendingWaveform { generation: 1, cancellation: cancellation.clone() },
        );
        state.deferred.push_back(WaveformJob {
            key,
            generation: 1,
            path: PathBuf::from("unused-shutdown-waveform-source.wav"),
            total_frames: 1,
            cancellation: cancellation.clone(),
        });
    }

    service.begin_shutdown();
    assert!(cancellation.is_canceled());
    let evidence = service.shutdown_until(std::time::Instant::now() + Duration::from_secs(2));
    assert_eq!(evidence.pending_requests_before, 1);
    assert_eq!(evidence.deferred_requests_before, 1);
    assert_eq!(evidence.pending_requests_remaining, 0);
    assert_eq!(evidence.deferred_requests_remaining, 0);
    assert!(evidence.source_cache.all_resources_released());
    assert!(evidence.all_resources_released(), "{evidence:#?}");
}

#[test]
fn default_and_stale_waveform_shutdown_evidence_fail_closed() {
    let clean = AudioWaveformService::new()
        .shutdown_until(std::time::Instant::now() + Duration::from_secs(2));
    assert!(clean.all_resources_released());
    assert!(!super::AudioWaveformShutdownEvidence::default().all_resources_released());
    assert!(
        !super::AudioWaveformShutdownEvidence { schema_version: 0, ..clean }
            .all_resources_released()
    );
    assert!(
        !super::AudioWaveformShutdownEvidence { worker_failures: 1, ..clean }
            .all_resources_released()
    );
}
