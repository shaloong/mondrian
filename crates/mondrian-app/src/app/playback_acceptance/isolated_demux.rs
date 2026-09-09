//! Fail-closed acceptance policy for the process-isolated Preview demux seam.

use super::{push_failure, PreviewAcceptanceFailure};
use crate::app::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics;
use serde::Serialize;

/// Aggregated, UI-independent process-isolated demux evidence for acceptance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(super) struct PreviewIsolatedDemuxGateEvidence {
    pub(super) session_launches: u64,
    ready_sessions: u64,
    cross_request_reused_sessions: u64,
    completed_seeks: u64,
    completed_reads: u64,
    packet_responses: u64,
    end_responses: u64,
    clean_closes: u64,
    cancellation_terminations: u64,
    failure_terminations: u64,
    forced_close_terminations: u64,
    pub(super) active_sessions: u64,
    per_worker_peak_sum: u64,
}

impl PreviewIsolatedDemuxGateEvidence {
    fn from_workers(workers: PreviewDecodeWorkerExecutionDiagnostics) -> Self {
        let mut aggregate = Self::default();
        for progress in [
            workers.any,
            workers.playback,
            workers.non_playback,
            workers.still,
        ]
        .into_iter()
        .flatten()
        {
            aggregate.accumulate(progress.isolated_demux);
        }
        aggregate
    }

    fn accumulate(&mut self, evidence: mondrian_media::PreviewIsolatedDemuxExecutionEvidence) {
        self.session_launches = self.session_launches.saturating_add(evidence.session_launches);
        self.ready_sessions = self.ready_sessions.saturating_add(evidence.ready_sessions);
        self.cross_request_reused_sessions = self
            .cross_request_reused_sessions
            .saturating_add(evidence.cross_request_reused_sessions);
        self.completed_seeks = self.completed_seeks.saturating_add(evidence.completed_seeks);
        self.completed_reads = self.completed_reads.saturating_add(evidence.completed_reads);
        self.packet_responses = self.packet_responses.saturating_add(evidence.packet_responses);
        self.end_responses = self.end_responses.saturating_add(evidence.end_responses);
        self.clean_closes = self.clean_closes.saturating_add(evidence.clean_closes);
        self.cancellation_terminations = self
            .cancellation_terminations
            .saturating_add(evidence.cancellation_terminations);
        self.failure_terminations =
            self.failure_terminations.saturating_add(evidence.failure_terminations);
        self.forced_close_terminations = self
            .forced_close_terminations
            .saturating_add(evidence.forced_close_terminations);
        self.active_sessions = self.active_sessions.saturating_add(evidence.active_sessions);
        self.per_worker_peak_sum =
            self.per_worker_peak_sum.saturating_add(evidence.peak_active_sessions);
    }

    const fn reaped_sessions(self) -> u64 {
        self.clean_closes
            .saturating_add(self.cancellation_terminations)
            .saturating_add(self.failure_terminations)
            .saturating_add(self.forced_close_terminations)
    }
}

pub(super) fn evaluate_isolated_demux(
    workers: PreviewDecodeWorkerExecutionDiagnostics,
    failures: &mut Vec<PreviewAcceptanceFailure>,
) -> PreviewIsolatedDemuxGateEvidence {
    let evidence = PreviewIsolatedDemuxGateEvidence::from_workers(workers);
    if evidence.session_launches == 0 {
        push_failure(
            failures,
            "isolated_demux_not_executed",
            "at least one successfully launched packaged demux helper",
            "0 launches",
            "Preview Decode Execution Progress helper lifecycle facts",
        );
    }
    if evidence.ready_sessions == 0 {
        push_failure(
            failures,
            "isolated_demux_stream_contract_unproven",
            "at least one helper with a validated stream contract",
            "0 ready sessions",
            "Preview Decode Execution Progress helper lifecycle facts",
        );
    }
    if evidence.cross_request_reused_sessions == 0 {
        push_failure(
            failures,
            "isolated_demux_cross_request_reuse_unproven",
            "at least one helper completing demux work for a later decode request",
            "0 cross-request reused sessions",
            "Preview Decode Execution Progress request and helper identities",
        );
    }
    if evidence.completed_seeks == 0 {
        push_failure(
            failures,
            "isolated_demux_seek_unproven",
            "at least one acknowledged helper seek",
            "0 completed seeks",
            "Preview Decode Execution Progress helper command facts",
        );
    }
    if evidence.completed_reads == 0 || evidence.packet_responses == 0 {
        push_failure(
            failures,
            "isolated_demux_packet_execution_unproven",
            "at least one completed helper read with a validated packet",
            format!(
                "completed_reads={}, packet_responses={}",
                evidence.completed_reads, evidence.packet_responses
            ),
            "Preview Decode Execution Progress helper command facts",
        );
    }
    if evidence.completed_reads != evidence.packet_responses.saturating_add(evidence.end_responses)
    {
        push_failure(
            failures,
            "isolated_demux_read_accounting_inconsistent",
            "completed reads equal packet plus end responses",
            format!(
                "completed_reads={}, packets={}, ends={}",
                evidence.completed_reads, evidence.packet_responses, evidence.end_responses
            ),
            "Preview Decode Execution Progress helper command facts",
        );
    }
    if evidence.ready_sessions > evidence.session_launches
        || evidence.cross_request_reused_sessions > evidence.ready_sessions
    {
        push_failure(
            failures,
            "isolated_demux_lifecycle_accounting_inconsistent",
            "reused sessions <= ready sessions <= launches",
            format!(
                "launches={}, ready={}, reused={}",
                evidence.session_launches,
                evidence.ready_sessions,
                evidence.cross_request_reused_sessions
            ),
            "Preview Decode Execution Progress helper lifecycle facts",
        );
    }
    if evidence.active_sessions > 0 || evidence.reaped_sessions() != evidence.session_launches {
        push_failure(
            failures,
            "isolated_demux_not_fully_reaped",
            "0 active helpers and exactly one reaped terminal outcome per launch",
            format!(
                "launches={}, reaped={}, active={}",
                evidence.session_launches,
                evidence.reaped_sessions(),
                evidence.active_sessions
            ),
            "post-stress Preview worker lifecycle snapshot",
        );
    }
    if evidence.clean_closes == 0 {
        push_failure(
            failures,
            "isolated_demux_clean_close_unproven",
            "at least one acknowledged and bounded clean helper close",
            "0 clean closes",
            "post-stress Preview worker lifecycle snapshot",
        );
    }
    if evidence.failure_terminations > 0 {
        push_failure(
            failures,
            "isolated_demux_failure_termination_observed",
            "0 protocol, process, pipe, or FFmpeg failure terminations",
            evidence.failure_terminations.to_string(),
            "Preview Decode Execution Progress helper terminal outcomes",
        );
    }
    if evidence.forced_close_terminations > 0 {
        push_failure(
            failures,
            "isolated_demux_forced_close_observed",
            "0 forced terminations of healthy helpers during bounded close",
            evidence.forced_close_terminations.to_string(),
            "Preview Decode Execution Progress helper terminal outcomes",
        );
    }
    evidence
}
