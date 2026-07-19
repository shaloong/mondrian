use std::path::PathBuf;
use std::time::Duration;

use mondrian_core::{AssetId, ExecutionCancellationToken, ExecutionTerminalDisposition};

use super::analysis::resample_peaks;
use super::state::{
    duration_to_waveform_frames, push_terminal, rotate_waveform_generation, PendingWaveform,
    WaveformFailure, WaveformJob, WaveformSourceKey, WaveformState,
};
use super::{
    AudioWaveformService, WaveformFailureReason, WAVEFORM_MAX_WIDTH,
    WAVEFORM_TERMINAL_EVIDENCE_CAPACITY,
};

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
    let key = WaveformSourceKey { asset_id: AssetId::new(), source_revision: 7 };
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
    let key = WaveformSourceKey { asset_id: AssetId::new(), source_revision: 1 };
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
    assert_eq!(service.source().lookup(asset_id, 1, 0.0, 1.0, 100), None);

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
    let key = WaveformSourceKey { asset_id: AssetId::new(), source_revision: 9 };
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
