//! Fail-closed acceptance policy for the production CPAL A/V playback path.
//!
//! This Module interprets facts collected by the normal App, Playback, Media,
//! platform, and headless Viewer Interfaces. It does not own transport policy,
//! schedule work, or provide a test-only audio implementation.

use super::playback_acceptance::{
    evaluate_process_memory_gate, PreviewProcessMemoryEvidenceReport,
    PreviewProcessMemoryGateReport, PROFESSIONAL_MIN_OBSERVED_DURATION_US,
};
#[cfg(feature = "validation")]
use mondrian_media::MediaInfo;
use mondrian_media::{
    AudioPlaybackSnapshot, AudioPlaybackState, AudioSourceCacheDiagnostics,
    RealtimeAudioOutputLossReason,
};
use mondrian_playback::{
    PlaybackClockPhaseErrorSummary, PlaybackEvidenceReport, PLAYBACK_EVIDENCE_SCHEMA_VERSION,
};
use serde::Serialize;

const OUTPUT_SAMPLE_RATE: u32 = 48_000;
const OUTPUT_CHANNELS: u8 = 2;
const MAX_CALLBACK_TIMELINE_DIVERGENCE_FLOOR_US: u64 = 100_000;
const MAX_CALLBACK_CLOCK_RATE_ERROR_PPM: u64 = 1_000;
const MAX_CALLBACK_AGE_US: u64 = 100_000;
const MAX_DELIVERY_PHASE_ERROR_US: u64 = 20_000;
const MAX_SYNTHETIC_FALLBACK_US: u64 = 1_000_000;
const MAX_RECOVERY_HANDOFF_US: u64 = 5_000_000;
const MAX_RECOVERY_SYNTHETIC_RESIDENCY_US: u64 = MAX_RECOVERY_HANDOFF_US;
const MIN_STABLE_CLOCK_RESIDENCY_US: u64 = 1_000_000;
const MIN_VIDEO_READY_BASIS_POINTS: u64 = 9_950;
const MAX_AUDIO_WINDOW_DECODE_US: u64 = 460_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AudioPlaybackMediaProbeReport {
    source: &'static str,
    duration_us: u64,
    audio_stream_duration_us: Option<u64>,
    source_sample_rate: u32,
    source_channels: u8,
}

impl AudioPlaybackMediaProbeReport {
    #[cfg(feature = "validation")]
    pub(crate) fn from_media_info(media_info: &MediaInfo) -> anyhow::Result<Self> {
        let audio = media_info
            .primary_audio()
            .ok_or_else(|| anyhow::anyhow!("FFmpeg media probe found no audio stream"))?;
        anyhow::ensure!(
            audio.sample_rate > 0 && audio.channels > 0,
            "FFmpeg media probe did not resolve a positive audio contract"
        );
        Ok(Self {
            source: "ffmpeg_avformat_decoder_probe",
            duration_us: media_info.duration.as_micros().min(u128::from(u64::MAX)) as u64,
            audio_stream_duration_us: audio
                .duration
                .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64),
            source_sample_rate: audio.sample_rate,
            source_channels: audio.channels,
        })
    }

    pub(crate) fn ensure_observation_coverage(
        &self,
        required_duration_us: u64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            required_duration_us >= PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            "professional CPAL playback observation cannot be shorter than {PROFESSIONAL_MIN_OBSERVED_DURATION_US} us"
        );
        anyhow::ensure!(
            self.duration_us >= required_duration_us,
            "professional CPAL playback requires at least {} us of source audio, but the probe provides {} us",
            required_duration_us,
            self.duration_us
        );
        let audio_stream_duration_us = self.audio_stream_duration_us.ok_or_else(|| {
            anyhow::anyhow!(
                "professional CPAL playback requires a proven primary-audio stream duration"
            )
        })?;
        anyhow::ensure!(
            audio_stream_duration_us >= required_duration_us,
            "professional CPAL playback requires at least {required_duration_us} us of primary-audio stream duration, but the probe provides {audio_stream_duration_us} us"
        );
        Ok(())
    }
}

pub(crate) struct ProfessionalAudioPlaybackObservation<'a> {
    pub(crate) media: &'a AudioPlaybackMediaProbeReport,
    pub(crate) recovery: ProfessionalAudioRecoveryObservation,
    pub(crate) audio: AudioPlaybackSnapshot,
    pub(crate) source_cache: AudioSourceCacheDiagnostics,
    pub(crate) playback_evidence: &'a PlaybackEvidenceReport,
    pub(crate) process_memory: &'a PreviewProcessMemoryEvidenceReport,
    pub(crate) video_readiness: ProfessionalVideoReadinessObservation,
    pub(crate) video_coordinator: ProfessionalVideoCoordinatorObservation,
    pub(crate) gpu_presented_frames: u64,
}

/// Exhaustive classification of sampled current-frame Viewer outcomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProfessionalVideoReadinessObservation {
    pub(crate) ready: u64,
    pub(crate) loading: u64,
    pub(crate) stale: u64,
    pub(crate) unavailable: u64,
    pub(crate) missed_deadline: u64,
}

impl ProfessionalVideoReadinessObservation {
    fn total(self) -> u64 {
        self.ready
            .saturating_add(self.loading)
            .saturating_add(self.stale)
            .saturating_add(self.unavailable)
            .saturating_add(self.missed_deadline)
    }
}

/// Long-run scheduling shape behind sampled Headless Viewer readiness.
///
/// Aggregate readiness alone cannot distinguish isolated operating-system
/// jitter from a sustained execution stall. Candidate-state counts remain
/// mutually exclusive and cover every non-ready interval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ProfessionalVideoCoordinatorObservation {
    pub(crate) intervals: u64,
    pub(crate) stale_intervals: u64,
    pub(crate) stale_bursts: u64,
    pub(crate) max_consecutive_stale: u64,
    pub(crate) stale_ready: u64,
    pub(crate) stale_queued_ready: u64,
    pub(crate) stale_in_flight: u64,
    pub(crate) stale_loading: u64,
    pub(crate) stale_backpressured: u64,
    pub(crate) stale_dropped_late: u64,
    pub(crate) stale_unavailable: u64,
}

impl ProfessionalVideoCoordinatorObservation {
    fn classified_stale(self) -> u64 {
        self.stale_ready
            .saturating_add(self.stale_queued_ready)
            .saturating_add(self.stale_in_flight)
            .saturating_add(self.stale_loading)
            .saturating_add(self.stale_backpressured)
            .saturating_add(self.stale_dropped_late)
            .saturating_add(self.stale_unavailable)
    }
}

/// Monotonic and lifecycle facts surrounding the validation-only device recycle.
///
/// The retained Audio Playback lifecycle is the authority for what happened;
/// these durations only bind its milestones to the request's monotonic instant.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProfessionalAudioRecoveryObservation {
    pub(crate) initial_audio: AudioPlaybackSnapshot,
    pub(crate) initial_audio_device_stable_us: u64,
    pub(crate) request_to_loss_us: Option<u64>,
    pub(crate) request_to_synthetic_us: Option<u64>,
    pub(crate) request_to_reopen_us: Option<u64>,
    pub(crate) request_to_recovered_us: Option<u64>,
    pub(crate) final_audio_device_stable_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AudioPlaybackAcceptanceFailure {
    code: &'static str,
    expected: String,
    observed: String,
    evidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProfessionalAudioPlaybackGateReport {
    profile: &'static str,
    media: AudioPlaybackMediaProbeReport,
    required_observed_duration_us: u64,
    observed_duration_us: u64,
    audio_device_residency_us: u64,
    synthetic_residency_us: u64,
    output_state: &'static str,
    initial_stream_generation: Option<u64>,
    final_stream_generation: Option<u64>,
    request_to_loss_us: Option<u64>,
    request_to_synthetic_us: Option<u64>,
    request_to_reopen_us: Option<u64>,
    request_to_recovered_us: Option<u64>,
    initial_audio_device_stable_us: u64,
    final_audio_device_stable_us: u64,
    lifecycle_opened_delta: Option<u64>,
    lifecycle_lost_delta: Option<u64>,
    lifecycle_controlled_recycle_delta: Option<u64>,
    lifecycle_backend_loss_delta: Option<u64>,
    lifecycle_deactivation_failed_delta: Option<u64>,
    frozen_loss_generation: Option<u64>,
    frozen_loss_anchor_sample: Option<i64>,
    frozen_loss_anchor_rate: Option<u32>,
    output_sample_rate: Option<u32>,
    output_channels: Option<u8>,
    active_callback_consumed_frames: u64,
    active_callback_duration_us: u64,
    callback_timeline_divergence_us: u64,
    callback_count: u64,
    output_underrun_frames: u64,
    render_substitutions: u64,
    underrun_recoveries: u64,
    audio_device_delivery_phase: PlaybackClockPhaseErrorSummary,
    synthetic_delivery_phase: PlaybackClockPhaseErrorSummary,
    unproven_presentable_deliveries: u64,
    phase_not_applicable_deliveries: u64,
    video_ready_samples: u64,
    video_loading_samples: u64,
    video_stale_samples: u64,
    video_unavailable_samples: u64,
    video_missed_deadline_samples: u64,
    video_total_samples: u64,
    video_ready_basis_points: u64,
    video_coordinator: ProfessionalVideoCoordinatorObservation,
    gpu_presented_frames: u64,
    source_cache: AudioSourceCacheDiagnostics,
    process_memory: PreviewProcessMemoryGateReport,
    pub(crate) passed: bool,
    pub(crate) failures: Vec<AudioPlaybackAcceptanceFailure>,
}

pub(crate) fn evaluate_professional_audio_playback(
    observation: ProfessionalAudioPlaybackObservation<'_>,
) -> ProfessionalAudioPlaybackGateReport {
    let mut failures = Vec::new();
    let evidence = observation.playback_evidence;
    let audio = observation.audio;
    let output = audio.output;
    let recovery = observation.recovery;
    let initial_audio = recovery.initial_audio;
    let initial_output = initial_audio.output;
    let initial_stream_generation = initial_output.map(|output| output.stream_generation);
    let final_stream_generation = output.map(|output| output.stream_generation);
    let initial_lifecycle = initial_audio.output_lifecycle;
    let final_lifecycle = audio.output_lifecycle;
    let opened_delta =
        checked_counter_delta(final_lifecycle.opened_count, initial_lifecycle.opened_count);
    let lost_delta =
        checked_counter_delta(final_lifecycle.lost_count, initial_lifecycle.lost_count);
    let controlled_recycle_delta = checked_counter_delta(
        final_lifecycle.controlled_recycle_count,
        initial_lifecycle.controlled_recycle_count,
    );
    let backend_loss_delta = checked_counter_delta(
        final_lifecycle.backend_loss_count,
        initial_lifecycle.backend_loss_count,
    );
    let deactivation_failed_delta = checked_counter_delta(
        final_lifecycle.deactivation_failed_count,
        initial_lifecycle.deactivation_failed_count,
    );
    let frozen_loss = final_lifecycle.last_loss;
    let process_memory = evaluate_process_memory_gate(observation.process_memory);

    require(
        &mut failures,
        evidence.schema_version == PLAYBACK_EVIDENCE_SCHEMA_VERSION,
        "playback_evidence_schema_mismatch",
        PLAYBACK_EVIDENCE_SCHEMA_VERSION.to_string(),
        evidence.schema_version.to_string(),
        "versioned Playback Evidence report",
    );
    require(
        &mut failures,
        evidence.observed_duration_us >= PROFESSIONAL_MIN_OBSERVED_DURATION_US,
        "playback_observation_too_short",
        format!("at least {PROFESSIONAL_MIN_OBSERVED_DURATION_US} us"),
        format!("{} us", evidence.observed_duration_us),
        "bounded Playback Evidence residency",
    );
    let required_audio_residency = evidence
        .observed_duration_us
        .saturating_sub(MAX_RECOVERY_SYNTHETIC_RESIDENCY_US);
    require(
        &mut failures,
        evidence.clock_residency.audio_device_us >= required_audio_residency,
        "audio_device_clock_residency_below_minimum",
        format!("at least {required_audio_residency} us"),
        format!("{} us", evidence.clock_residency.audio_device_us),
        "Playback Clock Master residency outside the bounded controlled-recovery interval",
    );
    require(
        &mut failures,
        evidence.clock_residency.synthetic_us > 0
            && evidence.clock_residency.synthetic_us <= MAX_RECOVERY_SYNTHETIC_RESIDENCY_US,
        "synthetic_clock_residency_outside_recovery_limit",
        format!("one or more and at most {MAX_RECOVERY_SYNTHETIC_RESIDENCY_US} us"),
        format!("{} us", evidence.clock_residency.synthetic_us),
        "Synthetic Clock Master residency during controlled device recovery",
    );
    require(
        &mut failures,
        evidence.clock_residency.none_us == 0,
        "missing_clock_master_residency_observed",
        "zero time without an authoritative Clock Master",
        format!("{} us", evidence.clock_residency.none_us),
        "Audio Device to Synthetic to Audio Device Clock handoff",
    );
    require_phase_evidence(
        &mut failures,
        "audio_device",
        evidence.delivery_phase_error.audio_device,
        true,
    );
    require_phase_evidence(
        &mut failures,
        "synthetic",
        evidence.delivery_phase_error.synthetic,
        true,
    );
    require(
        &mut failures,
        evidence.delivery_phase_error.unproven_presentable == 0,
        "unproven_presentable_delivery_phase",
        "zero running presentable deliveries without proven Clock phase",
        evidence.delivery_phase_error.unproven_presentable.to_string(),
        "completion-time Engine Frame Delivery application evidence",
    );

    require(
        &mut failures,
        initial_audio.state == AudioPlaybackState::Active
            && initial_output.is_some_and(|output| {
                output.active && !output.stream_failed && output.active_callback_consumed_frames > 0
            }),
        "initial_audio_device_not_qualified",
        "healthy active real callback consumption before recycle",
        format!("state={:?}, output={initial_output:?}", initial_audio.state),
        "initial production Audio Playback snapshot after stable qualification",
    );
    require(
        &mut failures,
        recovery.initial_audio_device_stable_us >= MIN_STABLE_CLOCK_RESIDENCY_US,
        "initial_audio_device_stability_too_short",
        format!("at least {MIN_STABLE_CLOCK_RESIDENCY_US} us"),
        format!("{} us", recovery.initial_audio_device_stable_us),
        "pre-recycle Audio Device Clock Master/Active stability interval",
    );
    require(
        &mut failures,
        recovery.final_audio_device_stable_us >= MIN_STABLE_CLOCK_RESIDENCY_US,
        "final_audio_device_stability_too_short",
        format!("at least {MIN_STABLE_CLOCK_RESIDENCY_US} us"),
        format!("{} us", recovery.final_audio_device_stable_us),
        "post-recovery Audio Device Clock Master/Active stability interval",
    );
    require_recovery_latency(
        &mut failures,
        "controlled_recycle_loss",
        recovery.request_to_loss_us,
        MAX_RECOVERY_HANDOFF_US,
    );
    require_recovery_latency(
        &mut failures,
        "synthetic_clock_fallback",
        recovery.request_to_synthetic_us,
        MAX_SYNTHETIC_FALLBACK_US,
    );
    require_recovery_latency(
        &mut failures,
        "replacement_stream_open",
        recovery.request_to_reopen_us,
        MAX_RECOVERY_HANDOFF_US,
    );
    require_recovery_latency(
        &mut failures,
        "audio_device_phase_handoff",
        recovery.request_to_recovered_us,
        MAX_RECOVERY_HANDOFF_US,
    );
    require(
        &mut failures,
        opened_delta == Some(1) && lost_delta == Some(1) && controlled_recycle_delta == Some(1),
        "controlled_recycle_lifecycle_delta_mismatch",
        "exactly one opened, lost, and controlled-recycle lifecycle transition",
        format!(
            "opened={opened_delta:?}, lost={lost_delta:?}, controlled={controlled_recycle_delta:?}"
        ),
        "retained Audio Playback output lifecycle counters",
    );
    require(
        &mut failures,
        backend_loss_delta == Some(0) && deactivation_failed_delta == Some(0),
        "unexpected_audio_output_loss_observed",
        "zero backend-loss and deactivation-failure transitions",
        format!(
            "backend={backend_loss_delta:?}, deactivation_failed={deactivation_failed_delta:?}"
        ),
        "retained Audio Playback output lifecycle counters",
    );
    require(
        &mut failures,
        initial_stream_generation.zip(final_stream_generation).is_some_and(
            |(initial, final_generation)| {
                final_generation > initial
                    && final_lifecycle.last_lost_generation == Some(initial)
                    && final_lifecycle.last_opened_generation == Some(final_generation)
            },
        ),
        "audio_output_generation_recovery_mismatch",
        "newer terminal generation with initial generation lost and terminal generation opened",
        format!(
            "initial={initial_stream_generation:?}, final={final_stream_generation:?}, last_lost={:?}, last_opened={:?}",
            final_lifecycle.last_lost_generation, final_lifecycle.last_opened_generation
        ),
        "concrete stream generation and retained lifecycle lineage",
    );
    let frozen_loss_valid = frozen_loss.zip(initial_output).is_some_and(|(loss, initial)| {
        loss.reason == RealtimeAudioOutputLossReason::ControlledRecycle
            && loss.final_output.stream_generation == initial.stream_generation
            && loss.final_output.contract == initial.contract
            && loss.final_output.callback_count >= initial.callback_count
            && loss.final_output.active_callback_consumed_frames
                >= initial.active_callback_consumed_frames
            && !loss.final_output.active
            && loss
                .final_media_anchor
                .is_some_and(|anchor| anchor.rate().hz() == OUTPUT_SAMPLE_RATE)
    });
    require(
        &mut failures,
        frozen_loss_valid,
        "controlled_recycle_frozen_loss_invalid",
        "post-drop inactive snapshot and exact 48 kHz media anchor for the initial generation",
        format!("{frozen_loss:?}"),
        "retained post-drop Audio Playback loss snapshot",
    );
    require(
        &mut failures,
        evidence.deliveries.rejected == 0
            && evidence.deliveries.failed == 0
            && evidence.deliveries.blocked == 0,
        "invalid_terminal_delivery_observed",
        "zero rejected, failed, or blocked terminal deliveries",
        format!("{:?}", evidence.deliveries),
        "Playback Evidence delivery aggregates",
    );
    require(
        &mut failures,
        evidence.audio_underrun_frames == 0
            && evidence.audio_underrun_recoveries == 0
            && audio.underrun_recovery_count == 0,
        "audio_underrun_recovery_observed",
        "zero sustained-underrun recovery cycles",
        format!(
            "frames={}, evidence_recoveries={}, runtime_recoveries={}",
            evidence.audio_underrun_frames,
            evidence.audio_underrun_recoveries,
            audio.underrun_recovery_count
        ),
        "Playback Evidence and Audio Playback lifecycle",
    );
    require(
        &mut failures,
        audio.state == AudioPlaybackState::Active,
        "audio_playback_not_active",
        "Active",
        audio_state_name(audio.state),
        "production Audio Playback snapshot",
    );
    require(
        &mut failures,
        audio.render_substitution_count == 0 && audio.render_generation_recovery_count == 0,
        "audio_render_failure_observed",
        "zero exact-duration silence substitutions and render-generation recoveries",
        format!(
            "substitutions={}, generation_recoveries={}",
            audio.render_substitution_count, audio.render_generation_recovery_count
        ),
        "production Audio Playback render lifecycle",
    );

    let mut callback_consumed_frames = 0;
    let mut callback_duration_us = 0;
    let mut callback_divergence_us = u64::MAX;
    let mut callback_count = 0;
    let mut output_underrun_frames = 0;
    if let Some(output) = output {
        callback_consumed_frames = output.active_callback_consumed_frames;
        callback_duration_us = output
            .active_duration
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0);
        let callback_position_us = callback_consumed_frames
            .saturating_mul(1_000_000)
            .checked_div(u64::from(output.contract.sample_rate.max(1)))
            .unwrap_or(u64::MAX);
        callback_divergence_us = callback_position_us.abs_diff(callback_duration_us);
        let callback_divergence_limit_us = MAX_CALLBACK_TIMELINE_DIVERGENCE_FLOOR_US.max(
            callback_duration_us
                .saturating_mul(MAX_CALLBACK_CLOCK_RATE_ERROR_PPM)
                .checked_div(1_000_000)
                .unwrap_or(u64::MAX),
        );
        callback_count = output.callback_count;
        output_underrun_frames = output.underrun_frames;
        let callback_age_us = output
            .last_callback_age
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(u64::MAX);
        require(
            &mut failures,
            Some(output.stream_generation) == final_stream_generation,
            "terminal_audio_stream_generation_mismatch",
            format!("{final_stream_generation:?}"),
            output.stream_generation.to_string(),
            "terminal Audio Playback and concrete CPAL stream generations",
        );
        require(
            &mut failures,
            output.contract.sample_rate == OUTPUT_SAMPLE_RATE
                && output.contract.channels() == OUTPUT_CHANNELS,
            "audio_output_contract_mismatch",
            format!("{OUTPUT_SAMPLE_RATE} Hz / {OUTPUT_CHANNELS} channels"),
            format!(
                "{} Hz / {} channels",
                output.contract.sample_rate,
                output.contract.channels()
            ),
            "concrete CPAL output stream",
        );
        require(
            &mut failures,
            output.active && !output.stream_failed,
            "audio_output_stream_not_healthy",
            "active stream without asynchronous failure",
            format!("active={}, failed={}", output.active, output.stream_failed),
            "CPAL callback telemetry",
        );
        require(
            &mut failures,
            callback_age_us <= MAX_CALLBACK_AGE_US,
            "audio_callback_stale",
            format!("at most {MAX_CALLBACK_AGE_US} us old"),
            format!("{callback_age_us} us"),
            "latest concrete CPAL output callback",
        );
        require(
            &mut failures,
            callback_duration_us >= PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            "audio_callback_observation_too_short",
            format!("at least {PROFESSIONAL_MIN_OBSERVED_DURATION_US} us"),
            format!("{callback_duration_us} us"),
            "wall duration of the current active CPAL consumption interval",
        );
        require(
            &mut failures,
            callback_count > 0 && callback_consumed_frames > 0,
            "audio_callback_consumption_missing",
            "non-zero callback count and active consumed frames",
            format!("callbacks={callback_count}, frames={callback_consumed_frames}"),
            "CPAL callback telemetry",
        );
        require(
            &mut failures,
            callback_divergence_us <= callback_divergence_limit_us,
            "audio_callback_cadence_diverged",
            format!(
                "at most {callback_divergence_limit_us} us ({MAX_CALLBACK_CLOCK_RATE_ERROR_PPM} ppm with {MAX_CALLBACK_TIMELINE_DIVERGENCE_FLOOR_US} us floor)"
            ),
            format!("{callback_divergence_us} us"),
            "callback-consumed frames versus active monotonic wall duration",
        );
        require(
            &mut failures,
            output.underrun_frames == 0 && audio.active_interval_underrun_frames == 0,
            "audio_output_underrun_observed",
            "zero callback and active-interval missing frames",
            format!(
                "callback={}, interval={}",
                output.underrun_frames, audio.active_interval_underrun_frames
            ),
            "CPAL callback and Audio Playback telemetry",
        );
    } else {
        push_failure(
            &mut failures,
            "audio_output_snapshot_missing",
            "one concrete CPAL output snapshot",
            "missing",
            "production Audio Playback snapshot",
        );
    }

    let video_total_samples = observation.video_readiness.total();
    let ready_basis_points = basis_points(observation.video_readiness.ready, video_total_samples);
    let coordinator = observation.video_coordinator;
    require(
        &mut failures,
        coordinator.intervals == video_total_samples
            && coordinator.stale_intervals == observation.video_readiness.stale
            && coordinator.classified_stale() == coordinator.stale_intervals
            && coordinator.stale_bursts <= coordinator.stale_intervals
            && coordinator.max_consecutive_stale <= coordinator.stale_intervals
            && (coordinator.stale_intervals > 0
                || (coordinator.stale_bursts == 0 && coordinator.max_consecutive_stale == 0)),
        "video_coordinator_evidence_inconsistent",
        "coordinator intervals close against readiness and every stale interval has one candidate-state classification",
        format!("{coordinator:?}"),
        "headless realtime coordinator evidence",
    );
    require(
        &mut failures,
        video_total_samples > 0 && ready_basis_points >= MIN_VIDEO_READY_BASIS_POINTS,
        "video_readiness_below_minimum",
        format!("at least {MIN_VIDEO_READY_BASIS_POINTS} basis points"),
        format!(
            "{} basis points from {}/{} samples",
            ready_basis_points, observation.video_readiness.ready, video_total_samples
        ),
        "headless Viewer current-frame readiness",
    );
    require(
        &mut failures,
        observation.gpu_presented_frames > 0,
        "gpu_presentation_missing",
        "one or more completed headless GPU presentations",
        observation.gpu_presented_frames.to_string(),
        "headless Viewer execution completion",
    );

    let cache = observation.source_cache;
    require(
        &mut failures,
        cache.reserved_bytes <= cache.byte_budget && cache.entries <= cache.entry_capacity,
        "audio_source_cache_budget_exceeded",
        format!(
            "at most {} bytes and {} entries",
            cache.byte_budget, cache.entry_capacity
        ),
        format!(
            "{} bytes and {} entries",
            cache.reserved_bytes, cache.entries
        ),
        "global weighted decoded-source LRU",
    );
    require(
        &mut failures,
        cache.decode_successes > 0
            && cache.decode_failures == 0
            && cache.failures == 0
            && cache.in_flight_decodes == 0,
        "audio_source_decode_failure_observed",
        "successful bounded-window decodes, zero failures, and no in-flight decode at quiescence",
        format!(
            "successes={}, failures={}, retained_failures={}, in_flight={}",
            cache.decode_successes, cache.decode_failures, cache.failures, cache.in_flight_decodes
        ),
        "bounded decoded-source cache diagnostics",
    );
    require(
        &mut failures,
        cache.oversize_windows == 0,
        "audio_source_oversize_window_observed",
        "zero oversize decoded windows",
        cache.oversize_windows.to_string(),
        "bounded decoded-source cache diagnostics",
    );
    require(
        &mut failures,
        cache.decoder_session_capacity > 0
            && cache.decoder_sessions <= cache.decoder_session_capacity
            && cache.decoder_sessions_above_capacity == 0,
        "audio_decoder_session_budget_exceeded",
        "resident session slots converged within the current non-zero configured capacity",
        format!(
            "resident={}, historical_peak={}, capacity={}, above_capacity={}",
            cache.decoder_sessions,
            cache.decoder_peak_sessions,
            cache.decoder_session_capacity,
            cache.decoder_sessions_above_capacity
        ),
        "persistent audio decoder session pool",
    );
    require(
        &mut failures,
        cache.decoder_session_opens > 0 && cache.decoder_sequential_reuses > 0,
        "audio_decoder_steady_state_evidence_missing",
        "at least one session open and one sequential reuse",
        format!(
            "opens={}, sequential_reuses={}",
            cache.decoder_session_opens, cache.decoder_sequential_reuses
        ),
        "persistent audio decoder session diagnostics",
    );
    require(
        &mut failures,
        cache.decoder_cancellations == 0,
        "audio_decoder_cancellation_observed",
        "zero generation cancellations during continuous playback",
        cache.decoder_cancellations.to_string(),
        "persistent audio decoder session diagnostics",
    );
    require(
        &mut failures,
        cache.decoder_sequential_window_max_duration_us <= MAX_AUDIO_WINDOW_DECODE_US,
        "audio_source_steady_window_decode_above_realtime_budget",
        format!("at most {MAX_AUDIO_WINDOW_DECODE_US} us"),
        format!("{} us", cache.decoder_sequential_window_max_duration_us),
        "slowest sequential ten-second source window versus product output high-water duration",
    );
    if !process_memory.passed() {
        push_failure(
            &mut failures,
            "product_process_tree_memory_gate_failed",
            "product_process_tree_private_commit_v2 passes",
            "failed",
            "fixed-cadence complete Mondrian process-tree memory evidence",
        );
    }

    ProfessionalAudioPlaybackGateReport {
        profile: "cpal_av_48khz_30min_recovery_v2",
        media: observation.media.clone(),
        required_observed_duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
        observed_duration_us: evidence.observed_duration_us,
        audio_device_residency_us: evidence.clock_residency.audio_device_us,
        synthetic_residency_us: evidence.clock_residency.synthetic_us,
        output_state: audio_state_name(audio.state),
        initial_stream_generation,
        final_stream_generation,
        request_to_loss_us: recovery.request_to_loss_us,
        request_to_synthetic_us: recovery.request_to_synthetic_us,
        request_to_reopen_us: recovery.request_to_reopen_us,
        request_to_recovered_us: recovery.request_to_recovered_us,
        initial_audio_device_stable_us: recovery.initial_audio_device_stable_us,
        final_audio_device_stable_us: recovery.final_audio_device_stable_us,
        lifecycle_opened_delta: opened_delta,
        lifecycle_lost_delta: lost_delta,
        lifecycle_controlled_recycle_delta: controlled_recycle_delta,
        lifecycle_backend_loss_delta: backend_loss_delta,
        lifecycle_deactivation_failed_delta: deactivation_failed_delta,
        frozen_loss_generation: frozen_loss.map(|loss| loss.final_output.stream_generation),
        frozen_loss_anchor_sample: frozen_loss
            .and_then(|loss| loss.final_media_anchor)
            .map(|anchor| anchor.sample()),
        frozen_loss_anchor_rate: frozen_loss
            .and_then(|loss| loss.final_media_anchor)
            .map(|anchor| anchor.rate().hz()),
        output_sample_rate: output.map(|output| output.contract.sample_rate),
        output_channels: output.map(|output| output.contract.channels()),
        active_callback_consumed_frames: callback_consumed_frames,
        active_callback_duration_us: callback_duration_us,
        callback_timeline_divergence_us: callback_divergence_us,
        callback_count,
        output_underrun_frames,
        render_substitutions: audio.render_substitution_count,
        underrun_recoveries: audio.underrun_recovery_count,
        audio_device_delivery_phase: evidence.delivery_phase_error.audio_device,
        synthetic_delivery_phase: evidence.delivery_phase_error.synthetic,
        unproven_presentable_deliveries: evidence.delivery_phase_error.unproven_presentable,
        phase_not_applicable_deliveries: evidence.delivery_phase_error.phase_not_applicable,
        video_ready_samples: observation.video_readiness.ready,
        video_loading_samples: observation.video_readiness.loading,
        video_stale_samples: observation.video_readiness.stale,
        video_unavailable_samples: observation.video_readiness.unavailable,
        video_missed_deadline_samples: observation.video_readiness.missed_deadline,
        video_total_samples,
        video_ready_basis_points: ready_basis_points,
        video_coordinator: coordinator,
        gpu_presented_frames: observation.gpu_presented_frames,
        source_cache: cache,
        process_memory,
        passed: failures.is_empty(),
        failures,
    }
}

fn checked_counter_delta(final_value: u64, initial_value: u64) -> Option<u64> {
    final_value.checked_sub(initial_value)
}

fn require_recovery_latency(
    failures: &mut Vec<AudioPlaybackAcceptanceFailure>,
    milestone: &'static str,
    observed_us: Option<u64>,
    limit_us: u64,
) {
    require(
        failures,
        observed_us.is_some_and(|value| value <= limit_us),
        match milestone {
            "controlled_recycle_loss" => "controlled_recycle_loss_latency_exceeded",
            "synthetic_clock_fallback" => "synthetic_clock_fallback_latency_exceeded",
            "replacement_stream_open" => "replacement_stream_open_latency_exceeded",
            "audio_device_phase_handoff" => "audio_device_phase_handoff_latency_exceeded",
            _ => "unknown_recovery_milestone",
        },
        format!("observed and at most {limit_us} us"),
        observed_us.map_or_else(|| "missing".to_owned(), |value| format!("{value} us")),
        format!("monotonic duration from the exact-current recycle request to {milestone}"),
    );
}

fn require_phase_evidence(
    failures: &mut Vec<AudioPlaybackAcceptanceFailure>,
    master: &'static str,
    summary: PlaybackClockPhaseErrorSummary,
    samples_required: bool,
) {
    let counts_complete = summary.point_error.count == summary.uncertainty.count
        && summary.point_error.count == summary.proven_error.count
        && summary.point_error.sampled_count == summary.uncertainty.sampled_count
        && summary.point_error.sampled_count == summary.proven_error.sampled_count;
    require(
        failures,
        counts_complete && (!samples_required || summary.proven_error.count > 0),
        match master {
            "audio_device" => "audio_device_delivery_phase_evidence_missing",
            "synthetic" => "synthetic_delivery_phase_evidence_missing",
            _ => "unknown_delivery_phase_master",
        },
        if samples_required {
            "one or more complete point/uncertainty/proven phase samples"
        } else {
            "complete point/uncertainty/proven phase accounting"
        },
        format!("{summary:?}"),
        format!("accepted presentable deliveries under {master} Clock Master"),
    );
    require(
        failures,
        summary.proven_error.max_us <= MAX_DELIVERY_PHASE_ERROR_US
            && summary.proven_error.max_us >= summary.point_error.max_us
            && summary.proven_error.max_us >= summary.uncertainty.max_us,
        match master {
            "audio_device" => "audio_device_delivery_phase_error_above_limit",
            "synthetic" => "synthetic_delivery_phase_error_above_limit",
            _ => "unknown_delivery_phase_limit",
        },
        format!(
            "conservative proven max at most {MAX_DELIVERY_PHASE_ERROR_US} us and not below either component"
        ),
        format!("{summary:?}"),
        format!("point plus uncertainty phase bound under {master} Clock Master"),
    );
}

fn audio_state_name(state: AudioPlaybackState) -> &'static str {
    match state {
        AudioPlaybackState::ExecutionUnavailable => "ExecutionUnavailable",
        AudioPlaybackState::DeviceUnavailable => "DeviceUnavailable",
        AudioPlaybackState::Idle => "Idle",
        AudioPlaybackState::WaitingForSource => "WaitingForSource",
        AudioPlaybackState::Prerolling => "Prerolling",
        AudioPlaybackState::Recovering => "Recovering",
        AudioPlaybackState::RenderBlocked => "RenderBlocked",
        AudioPlaybackState::Active => "Active",
    }
}

fn basis_points(numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        0
    } else {
        numerator.saturating_mul(10_000) / denominator
    }
}

fn require(
    failures: &mut Vec<AudioPlaybackAcceptanceFailure>,
    condition: bool,
    code: &'static str,
    expected: impl Into<String>,
    observed: impl Into<String>,
    evidence: impl Into<String>,
) {
    if !condition {
        push_failure(failures, code, expected, observed, evidence);
    }
}

fn push_failure(
    failures: &mut Vec<AudioPlaybackAcceptanceFailure>,
    code: &'static str,
    expected: impl Into<String>,
    observed: impl Into<String>,
    evidence: impl Into<String>,
) {
    failures.push(AudioPlaybackAcceptanceFailure {
        code,
        expected: expected.into(),
        observed: observed.into(),
        evidence: evidence.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AudioSamplePosition, AudioSampleRate};
    use mondrian_media::audio::RealtimeAudioOutputSnapshot;
    use mondrian_media::{AudioOutputLifecycleDiagnostics, AudioOutputLossSnapshot};
    use mondrian_platform::{ProcessMemoryProbeBackend, ProcessMemoryProbeResult};
    use mondrian_playback::{
        PlaybackClockResidency, PlaybackDeliveryCounts, PlaybackDeliveryPhaseErrorReport,
        PlaybackEvidenceReport, PlaybackLatencySummary, PlaybackStateResidency,
    };
    use std::time::{Duration, Instant};

    fn output_snapshot(
        stream_generation: u64,
        active_duration: Duration,
    ) -> RealtimeAudioOutputSnapshot {
        let active_frames = active_duration.as_secs().saturating_mul(u64::from(OUTPUT_SAMPLE_RATE));
        RealtimeAudioOutputSnapshot {
            captured_at: Instant::now(),
            stream_generation,
            contract: mondrian_media::RealtimeAudioOutputContract {
                sample_rate: OUTPUT_SAMPLE_RATE,
                channel_layout: mondrian_core::AudioChannelLayout::Stereo,
                sample_format: mondrian_media::RealtimeAudioSampleFormat::F32,
                channel_semantics: mondrian_media::RealtimeAudioChannelSemantics::StereoConvention,
                supported_buffer_size: mondrian_media::RealtimeAudioSupportedBufferSize::Unknown,
                candidates: mondrian_media::RealtimeAudioCandidateCounts {
                    enumerated: 1,
                    matching_channels: 1,
                    matching_sample_rate: 1,
                    executable: 1,
                },
            },
            callback_consumed_frames: active_frames,
            active_callback_consumed_frames: active_frames,
            active_duration: Some(active_duration),
            callback_count: active_duration.as_secs().saturating_mul(100),
            underrun_frames: 0,
            last_callback_frames: 480,
            last_callback_playback_delay: Some(Duration::from_millis(10)),
            last_callback_age: Some(Duration::from_millis(1)),
            buffered_frames: 9_600,
            stream_failed: false,
            active: true,
        }
    }

    fn initial_audio_snapshot() -> AudioPlaybackSnapshot {
        AudioPlaybackSnapshot {
            generation: 7,
            in_flight: 0,
            next_start_sample: 48_000,
            media_anchor: Some(sample_position(0)),
            activation_preroll_satisfied: true,
            state: AudioPlaybackState::Active,
            output: Some(output_snapshot(11, Duration::from_secs(1))),
            render_substitution_count: 0,
            render_generation_recovery_count: 0,
            stale_completion_count: 0,
            canceled_render_count: 0,
            active_interval_underrun_frames: 0,
            underrun_recovery_count: 0,
            output_lifecycle: AudioOutputLifecycleDiagnostics {
                opened_count: 1,
                last_opened_generation: Some(11),
                ..AudioOutputLifecycleDiagnostics::default()
            },
        }
    }

    fn passing_audio_snapshot() -> AudioPlaybackSnapshot {
        let initial = initial_audio_snapshot();
        let mut frozen_output = initial.output.expect("initial output");
        frozen_output.callback_consumed_frames += 480;
        frozen_output.active_callback_consumed_frames += 480;
        frozen_output.callback_count += 1;
        frozen_output.active = false;
        AudioPlaybackSnapshot {
            generation: 9,
            in_flight: 0,
            next_start_sample: 86_500_000,
            media_anchor: Some(sample_position(48_000)),
            activation_preroll_satisfied: true,
            state: AudioPlaybackState::Active,
            output: Some(output_snapshot(12, Duration::from_secs(30 * 60))),
            render_substitution_count: 0,
            render_generation_recovery_count: 0,
            stale_completion_count: 0,
            canceled_render_count: 1,
            active_interval_underrun_frames: 0,
            underrun_recovery_count: 0,
            output_lifecycle: AudioOutputLifecycleDiagnostics {
                opened_count: 2,
                lost_count: 1,
                controlled_recycle_count: 1,
                backend_loss_count: 0,
                deactivation_failed_count: 0,
                default_device_change_count: 0,
                device_selection_change_count: 0,
                last_opened_generation: Some(12),
                last_lost_generation: Some(11),
                last_loss: Some(AudioOutputLossSnapshot {
                    reason: RealtimeAudioOutputLossReason::ControlledRecycle,
                    final_output: frozen_output,
                    final_media_anchor: Some(sample_position(48_000)),
                }),
            },
        }
    }

    fn sample_position(sample: i64) -> AudioSamplePosition {
        AudioSamplePosition::new(
            sample,
            AudioSampleRate::new(OUTPUT_SAMPLE_RATE).expect("sample rate"),
        )
    }

    fn latency(count: u64, max_us: u64) -> PlaybackLatencySummary {
        PlaybackLatencySummary {
            count,
            sampled_count: count.min(4_096),
            p50_us: max_us / 4,
            p95_us: max_us / 2,
            p99_us: max_us,
            max_us,
        }
    }

    fn phase_summary(
        count: u64,
        point_us: u64,
        uncertainty_us: u64,
    ) -> PlaybackClockPhaseErrorSummary {
        PlaybackClockPhaseErrorSummary {
            point_error: latency(count, point_us),
            uncertainty: latency(count, uncertainty_us),
            proven_error: latency(count, point_us + uncertainty_us),
        }
    }

    fn passing_playback_evidence() -> PlaybackEvidenceReport {
        PlaybackEvidenceReport {
            schema_version: PLAYBACK_EVIDENCE_SCHEMA_VERSION,
            first_epoch: Some(1),
            latest_epoch: Some(1),
            observed_duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US + 500_000,
            snapshot_count: 54_000,
            demand_count: 54_000,
            superseded_demand_count: 0,
            seek_superseded_count: 0,
            clock_residency: PlaybackClockResidency {
                audio_device_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
                synthetic_us: 500_000,
                none_us: 0,
            },
            state_residency: PlaybackStateResidency {
                playing_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US + 500_000,
                ..PlaybackStateResidency::default()
            },
            clock_frame_advances: Default::default(),
            deliveries: PlaybackDeliveryCounts {
                ready: 54_000,
                ..PlaybackDeliveryCounts::default()
            },
            demand_latency: PlaybackLatencySummary::default(),
            warm_seek_latency: PlaybackLatencySummary::default(),
            accurate_seek_latency: PlaybackLatencySummary::default(),
            delivery_phase_error: PlaybackDeliveryPhaseErrorReport {
                audio_device: phase_summary(53_980, 4_000, 1_000),
                synthetic: phase_summary(20, 8_000, 2_000),
                unproven_presentable: 0,
                phase_not_applicable: 3,
            },
            audio_underrun_frames: 0,
            audio_underrun_recoveries: 0,
            retained_event_count: 0,
            evicted_event_count: 0,
            events: Vec::new(),
        }
    }

    fn passing_recovery() -> ProfessionalAudioRecoveryObservation {
        ProfessionalAudioRecoveryObservation {
            initial_audio: initial_audio_snapshot(),
            initial_audio_device_stable_us: MIN_STABLE_CLOCK_RESIDENCY_US,
            request_to_loss_us: Some(10_000),
            request_to_synthetic_us: Some(15_000),
            request_to_reopen_us: Some(100_000),
            request_to_recovered_us: Some(400_000),
            final_audio_device_stable_us: MIN_STABLE_CLOCK_RESIDENCY_US,
        }
    }

    fn evaluate_core_contract(
        recovery: ProfessionalAudioRecoveryObservation,
        audio: AudioPlaybackSnapshot,
        evidence: &PlaybackEvidenceReport,
    ) -> ProfessionalAudioPlaybackGateReport {
        let media = AudioPlaybackMediaProbeReport {
            source: "test",
            duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            audio_stream_duration_us: Some(PROFESSIONAL_MIN_OBSERVED_DURATION_US),
            source_sample_rate: OUTPUT_SAMPLE_RATE,
            source_channels: OUTPUT_CHANNELS,
        };
        let memory = PreviewProcessMemoryEvidenceReport::default();
        evaluate_professional_audio_playback(ProfessionalAudioPlaybackObservation {
            media: &media,
            recovery,
            audio,
            source_cache: AudioSourceCacheDiagnostics::default(),
            playback_evidence: evidence,
            process_memory: &memory,
            video_readiness: ProfessionalVideoReadinessObservation {
                ready: 1,
                ..ProfessionalVideoReadinessObservation::default()
            },
            video_coordinator: ProfessionalVideoCoordinatorObservation {
                intervals: 1,
                ..ProfessionalVideoCoordinatorObservation::default()
            },
            gpu_presented_frames: 1,
        })
    }

    #[test]
    fn media_probe_rejects_short_audio() {
        let probe = AudioPlaybackMediaProbeReport {
            source: "test",
            duration_us: 1_000_000,
            audio_stream_duration_us: Some(1_000_000),
            source_sample_rate: 44_100,
            source_channels: 1,
        };
        let error = probe
            .ensure_observation_coverage(PROFESSIONAL_MIN_OBSERVED_DURATION_US)
            .expect_err("short audio must fail closed");
        assert!(error.to_string().contains("1800000000"));
    }

    #[test]
    fn accepts_complete_production_path_evidence() {
        let evidence = passing_playback_evidence();
        let mut memory_collector =
            super::super::playback_acceptance::PreviewProcessMemoryEvidenceCollector::default();
        let memory_sample = || {
            ProcessMemoryProbeResult::observed(
                mondrian_platform::ProcessMemoryScope::ProductProcessTree,
                ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                3,
                1,
                512 * 1024 * 1024,
                384 * 1024 * 1024,
                512 * 1024 * 1024,
            )
        };
        for second in 0..=30 * 60 {
            memory_collector.observe_playback(second * 1_000_000, memory_sample());
        }
        memory_collector.observe_post_stress(memory_sample());
        let memory = memory_collector.report();
        let media = AudioPlaybackMediaProbeReport {
            source: "test",
            duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            audio_stream_duration_us: Some(PROFESSIONAL_MIN_OBSERVED_DURATION_US),
            source_sample_rate: 48_000,
            source_channels: 2,
        };
        let report = evaluate_professional_audio_playback(ProfessionalAudioPlaybackObservation {
            media: &media,
            recovery: passing_recovery(),
            audio: passing_audio_snapshot(),
            source_cache: AudioSourceCacheDiagnostics {
                entries: 2,
                failures: 0,
                reserved_bytes: 2 * 48_000 * 2 * 4 * 10,
                byte_budget: 256 * 1024 * 1024,
                entry_capacity: 128,
                hits: 10_000,
                misses: 180,
                decode_successes: 180,
                decode_failures: 0,
                decode_total_duration_us: 18_000_000,
                decode_max_duration_us: 950_000,
                evictions: 178,
                budget_reconfigurations: 0,
                budget_trim_events: 0,
                budget_trimmed_entries: 0,
                budget_trimmed_bytes: 0,
                oversize_windows: 0,
                in_flight_decodes: 0,
                peak_in_flight_decodes: 1,
                decoder_sessions: 1,
                decoder_session_capacity: 8,
                decoder_peak_sessions: 1,
                decoder_session_opens: 1,
                decoder_sequential_reuses: 179,
                decoder_random_seek_restarts: 0,
                decoder_session_evictions: 0,
                decoder_capacity_reconfigurations: 0,
                decoder_capacity_trim_evictions: 0,
                decoder_sessions_above_capacity: 0,
                decoder_cancellations: 0,
                decoder_cold_window_max_duration_us: 950_000,
                decoder_sequential_window_max_duration_us: 80_000,
                decoder_random_seek_window_max_duration_us: 0,
            },
            playback_evidence: &evidence,
            process_memory: &memory,
            video_readiness: ProfessionalVideoReadinessObservation {
                ready: 53_900,
                stale: 100,
                ..ProfessionalVideoReadinessObservation::default()
            },
            video_coordinator: ProfessionalVideoCoordinatorObservation {
                intervals: 54_000,
                stale_intervals: 100,
                stale_bursts: 100,
                max_consecutive_stale: 1,
                stale_dropped_late: 100,
                ..ProfessionalVideoCoordinatorObservation::default()
            },
            gpu_presented_frames: 54_000,
        });
        assert!(report.passed, "{:?}", report.failures);
    }

    #[test]
    fn rejects_fake_or_incomplete_output_evidence() {
        let mut evidence = passing_playback_evidence();
        evidence.delivery_phase_error.synthetic = PlaybackClockPhaseErrorSummary::default();
        let memory = PreviewProcessMemoryEvidenceReport::default();
        let media = AudioPlaybackMediaProbeReport {
            source: "test",
            duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            audio_stream_duration_us: Some(PROFESSIONAL_MIN_OBSERVED_DURATION_US),
            source_sample_rate: 48_000,
            source_channels: 2,
        };
        let mut audio = passing_audio_snapshot();
        audio.output = None;
        audio.render_substitution_count = 1;
        let report = evaluate_professional_audio_playback(ProfessionalAudioPlaybackObservation {
            media: &media,
            recovery: passing_recovery(),
            audio,
            source_cache: AudioSourceCacheDiagnostics::default(),
            playback_evidence: &evidence,
            process_memory: &memory,
            video_readiness: ProfessionalVideoReadinessObservation::default(),
            video_coordinator: ProfessionalVideoCoordinatorObservation::default(),
            gpu_presented_frames: 0,
        });
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"audio_output_snapshot_missing"));
        assert!(codes.contains(&"audio_render_failure_observed"));
        assert!(codes.contains(&"synthetic_delivery_phase_evidence_missing"));
        assert!(codes.contains(&"video_readiness_below_minimum"));
        assert!(codes.contains(&"product_process_tree_memory_gate_failed"));
        assert!(!report.passed);
    }

    #[test]
    fn rejects_extra_loss_or_generation_reuse() {
        let mut initial = initial_audio_snapshot();
        initial.output_lifecycle.lost_count = 1;
        let mut recovery = passing_recovery();
        recovery.initial_audio = initial;
        let mut audio = passing_audio_snapshot();
        audio.output_lifecycle.backend_loss_count = 1;
        audio.output_lifecycle.opened_count = 3;
        audio.output.as_mut().expect("output").stream_generation = 11;
        let evidence = passing_playback_evidence();
        let report = evaluate_core_contract(recovery, audio, &evidence);
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"controlled_recycle_lifecycle_delta_mismatch"));
        assert!(codes.contains(&"unexpected_audio_output_loss_observed"));
        assert!(codes.contains(&"audio_output_generation_recovery_mismatch"));
    }

    #[test]
    fn rejects_slow_handoff_unproven_phase_and_phase_bound() {
        let mut recovery = passing_recovery();
        recovery.request_to_synthetic_us = Some(MAX_SYNTHETIC_FALLBACK_US + 1);
        recovery.request_to_recovered_us = Some(MAX_RECOVERY_HANDOFF_US + 1);
        let mut evidence = passing_playback_evidence();
        evidence.delivery_phase_error.unproven_presentable = 1;
        evidence.delivery_phase_error.audio_device = phase_summary(10, 19_000, 2_000);
        let report = evaluate_core_contract(recovery, passing_audio_snapshot(), &evidence);
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"synthetic_clock_fallback_latency_exceeded"));
        assert!(codes.contains(&"audio_device_phase_handoff_latency_exceeded"));
        assert!(codes.contains(&"unproven_presentable_delivery_phase"));
        assert!(codes.contains(&"audio_device_delivery_phase_error_above_limit"));
    }
}
