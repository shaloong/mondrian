//! Fail-closed acceptance policy for the production CPAL A/V playback path.
//!
//! This Module interprets facts collected by the normal App, Playback, Media,
//! platform, and headless Viewer Interfaces. It does not own transport policy,
//! schedule work, or provide a test-only audio implementation.

use super::playback_acceptance::{
    evaluate_process_memory_gate, PreviewProcessMemoryEvidenceReport,
    PreviewProcessMemoryGateReport, PROFESSIONAL_MIN_OBSERVED_DURATION_US,
};
use mondrian_media::{
    AudioPlaybackSnapshot, AudioPlaybackState, AudioSourceCacheDiagnostics, MediaInfo,
};
use mondrian_playback::{PlaybackEvidenceReport, PlaybackLatencySummary};
use serde::Serialize;

const OUTPUT_SAMPLE_RATE: u32 = 48_000;
const OUTPUT_CHANNELS: u8 = 2;
const MAX_CALLBACK_TIMELINE_DIVERGENCE_US: u64 = 100_000;
const MAX_CALLBACK_AGE_US: u64 = 100_000;
const MAX_DELIVERY_CLOCK_DRIFT_US: u64 = 20_000;
const MAX_SYNTHETIC_STARTUP_US: u64 = 5_000_000;
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
    pub(crate) qualified_stream_generation: u64,
    pub(crate) audio: AudioPlaybackSnapshot,
    pub(crate) source_cache: AudioSourceCacheDiagnostics,
    pub(crate) playback_evidence: &'a PlaybackEvidenceReport,
    pub(crate) process_memory: &'a PreviewProcessMemoryEvidenceReport,
    pub(crate) video_ready_samples: u64,
    pub(crate) video_total_samples: u64,
    pub(crate) gpu_presented_frames: u64,
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
    qualified_stream_generation: u64,
    final_stream_generation: Option<u64>,
    output_sample_rate: Option<u32>,
    output_channels: Option<u8>,
    active_callback_consumed_frames: u64,
    active_callback_duration_us: u64,
    callback_timeline_divergence_us: u64,
    callback_count: u64,
    output_underrun_frames: u64,
    render_substitutions: u64,
    underrun_recoveries: u64,
    delivery_clock_drift: PlaybackLatencySummary,
    video_ready_samples: u64,
    video_total_samples: u64,
    video_ready_basis_points: u64,
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
    let process_memory = evaluate_process_memory_gate(observation.process_memory);

    require(
        &mut failures,
        evidence.observed_duration_us >= PROFESSIONAL_MIN_OBSERVED_DURATION_US,
        "playback_observation_too_short",
        format!("at least {PROFESSIONAL_MIN_OBSERVED_DURATION_US} us"),
        format!("{} us", evidence.observed_duration_us),
        "bounded Playback Evidence residency",
    );
    let required_audio_residency =
        evidence.observed_duration_us.saturating_sub(MAX_SYNTHETIC_STARTUP_US);
    require(
        &mut failures,
        evidence.clock_residency.audio_device_us >= required_audio_residency,
        "audio_device_clock_residency_below_minimum",
        format!("at least {required_audio_residency} us"),
        format!("{} us", evidence.clock_residency.audio_device_us),
        "Playback Clock Master residency after bounded startup",
    );
    require(
        &mut failures,
        evidence.clock_residency.synthetic_us <= MAX_SYNTHETIC_STARTUP_US,
        "synthetic_clock_residency_above_startup_limit",
        format!("at most {MAX_SYNTHETIC_STARTUP_US} us"),
        format!("{} us", evidence.clock_residency.synthetic_us),
        "Synthetic Clock Master is fallback/startup authority, not normal audio playback authority",
    );
    require(
        &mut failures,
        evidence.delivery_clock_drift.count > 0
            && evidence.delivery_clock_drift.max_us <= MAX_DELIVERY_CLOCK_DRIFT_US,
        "av_delivery_clock_drift_above_limit",
        format!("one or more samples and max at most {MAX_DELIVERY_CLOCK_DRIFT_US} us"),
        format!(
            "count={}, max={} us",
            evidence.delivery_clock_drift.count, evidence.delivery_clock_drift.max_us
        ),
        "accepted presentation target versus authoritative playback clock",
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
        evidence.audio_underrun_recoveries == 0 && audio.underrun_recovery_count == 0,
        "audio_underrun_recovery_observed",
        "zero sustained-underrun recovery cycles",
        format!(
            "evidence={}, runtime={}",
            evidence.audio_underrun_recoveries, audio.underrun_recovery_count
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
        audio.render_substitution_count == 0,
        "audio_render_substitution_observed",
        "zero exact-duration silence substitutions",
        audio.render_substitution_count.to_string(),
        "production Audio Playback render completions",
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
            .checked_div(u64::from(output.sample_rate.max(1)))
            .unwrap_or(u64::MAX);
        callback_divergence_us = callback_position_us.abs_diff(callback_duration_us);
        callback_count = output.callback_count;
        output_underrun_frames = output.underrun_frames;
        let callback_age_us = output
            .last_callback_age
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(u64::MAX);
        require(
            &mut failures,
            output.stream_generation == observation.qualified_stream_generation,
            "audio_stream_generation_changed",
            observation.qualified_stream_generation.to_string(),
            output.stream_generation.to_string(),
            "qualified and terminal CPAL stream generations",
        );
        require(
            &mut failures,
            output.sample_rate == OUTPUT_SAMPLE_RATE && output.channels == OUTPUT_CHANNELS,
            "audio_output_contract_mismatch",
            format!("{OUTPUT_SAMPLE_RATE} Hz / {OUTPUT_CHANNELS} channels"),
            format!("{} Hz / {} channels", output.sample_rate, output.channels),
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
            callback_divergence_us <= MAX_CALLBACK_TIMELINE_DIVERGENCE_US,
            "audio_callback_cadence_diverged",
            format!("at most {MAX_CALLBACK_TIMELINE_DIVERGENCE_US} us"),
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

    let ready_basis_points = basis_points(
        observation.video_ready_samples,
        observation.video_total_samples,
    );
    require(
        &mut failures,
        observation.video_total_samples > 0 && ready_basis_points >= MIN_VIDEO_READY_BASIS_POINTS,
        "video_readiness_below_minimum",
        format!("at least {MIN_VIDEO_READY_BASIS_POINTS} basis points"),
        format!(
            "{} basis points from {}/{} samples",
            ready_basis_points, observation.video_ready_samples, observation.video_total_samples
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
        cache.reserved_bytes <= cache.byte_budget,
        "audio_source_cache_budget_exceeded",
        format!("at most {} bytes", cache.byte_budget),
        format!("{} bytes", cache.reserved_bytes),
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
            && cache.decoder_peak_sessions <= cache.decoder_session_capacity,
        "audio_decoder_session_budget_exceeded",
        "resident and peak session slots within a non-zero configured capacity",
        format!(
            "resident={}, peak={}, capacity={}",
            cache.decoder_sessions, cache.decoder_peak_sessions, cache.decoder_session_capacity
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
            "whole_process_memory_gate_failed",
            "whole_process_private_commit_v1 passes",
            "failed",
            "fixed-cadence native process-memory evidence",
        );
    }

    ProfessionalAudioPlaybackGateReport {
        profile: "cpal_av_48khz_30min_v1",
        media: observation.media.clone(),
        required_observed_duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
        observed_duration_us: evidence.observed_duration_us,
        audio_device_residency_us: evidence.clock_residency.audio_device_us,
        synthetic_residency_us: evidence.clock_residency.synthetic_us,
        output_state: audio_state_name(audio.state),
        qualified_stream_generation: observation.qualified_stream_generation,
        final_stream_generation: output.map(|output| output.stream_generation),
        output_sample_rate: output.map(|output| output.sample_rate),
        output_channels: output.map(|output| output.channels),
        active_callback_consumed_frames: callback_consumed_frames,
        active_callback_duration_us: callback_duration_us,
        callback_timeline_divergence_us: callback_divergence_us,
        callback_count,
        output_underrun_frames,
        render_substitutions: audio.render_substitution_count,
        underrun_recoveries: audio.underrun_recovery_count,
        delivery_clock_drift: evidence.delivery_clock_drift,
        video_ready_samples: observation.video_ready_samples,
        video_total_samples: observation.video_total_samples,
        video_ready_basis_points: ready_basis_points,
        gpu_presented_frames: observation.gpu_presented_frames,
        source_cache: cache,
        process_memory,
        passed: failures.is_empty(),
        failures,
    }
}

fn audio_state_name(state: AudioPlaybackState) -> &'static str {
    match state {
        AudioPlaybackState::DeviceUnavailable => "DeviceUnavailable",
        AudioPlaybackState::Idle => "Idle",
        AudioPlaybackState::WaitingForSource => "WaitingForSource",
        AudioPlaybackState::Prerolling => "Prerolling",
        AudioPlaybackState::Recovering => "Recovering",
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
    use mondrian_core::{FramePosition, Rational};
    use mondrian_media::audio::RealtimeAudioOutputSnapshot;
    use mondrian_platform::{ProcessMemoryProbeBackend, ProcessMemoryProbeResult};
    use mondrian_playback::{
        ClockMaster, MonotonicTimestamp, PlaybackClockResidency, PlaybackDeliveryCounts,
        PlaybackEvidenceCollector, PlaybackEvidenceReport, PlaybackStateResidency,
        PlaybackTimelineBinding,
    };
    use std::time::Duration;

    fn passing_audio_snapshot() -> AudioPlaybackSnapshot {
        AudioPlaybackSnapshot {
            generation: 7,
            in_flight: 0,
            next_start_sample: 86_500_000,
            media_anchor: None,
            activation_preroll_satisfied: true,
            state: AudioPlaybackState::Active,
            output: Some(RealtimeAudioOutputSnapshot {
                stream_generation: 11,
                sample_rate: OUTPUT_SAMPLE_RATE,
                channels: OUTPUT_CHANNELS,
                callback_consumed_frames: 86_400_000,
                active_callback_consumed_frames: 86_400_000,
                active_duration: Some(Duration::from_secs(30 * 60)),
                callback_count: 180_000,
                underrun_frames: 0,
                last_callback_frames: 480,
                last_callback_playback_delay: Some(Duration::from_millis(10)),
                last_callback_age: Some(Duration::from_millis(1)),
                buffered_frames: 9_600,
                stream_failed: false,
                active: true,
            }),
            render_substitution_count: 0,
            stale_completion_count: 0,
            canceled_render_count: 0,
            active_interval_underrun_frames: 0,
            underrun_recovery_count: 0,
        }
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
        let evidence = PlaybackEvidenceReport {
            schema_version: 2,
            first_epoch: Some(1),
            latest_epoch: Some(1),
            observed_duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            snapshot_count: 54_000,
            demand_count: 54_000,
            superseded_demand_count: 0,
            seek_superseded_count: 0,
            clock_residency: PlaybackClockResidency {
                audio_device_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
                synthetic_us: 0,
                none_us: 0,
            },
            state_residency: PlaybackStateResidency {
                playing_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
                ..PlaybackStateResidency::default()
            },
            deliveries: PlaybackDeliveryCounts {
                ready: 54_000,
                ..PlaybackDeliveryCounts::default()
            },
            demand_latency: PlaybackLatencySummary::default(),
            warm_seek_latency: PlaybackLatencySummary::default(),
            accurate_seek_latency: PlaybackLatencySummary::default(),
            delivery_clock_drift: PlaybackLatencySummary {
                count: 54_000,
                sampled_count: 4_096,
                p50_us: 1_000,
                p95_us: 4_000,
                p99_us: 8_000,
                max_us: 10_000,
            },
            audio_underrun_frames: 0,
            audio_underrun_recoveries: 0,
            retained_event_count: 0,
            evicted_event_count: 0,
            events: Vec::new(),
        };
        let mut memory_collector =
            super::super::playback_acceptance::PreviewProcessMemoryEvidenceCollector::default();
        let memory_sample = || {
            ProcessMemoryProbeResult::observed(
                ProcessMemoryProbeBackend::WindowsProcessStatus,
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
            qualified_stream_generation: 11,
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
                decoder_cancellations: 0,
                decoder_cold_window_max_duration_us: 950_000,
                decoder_sequential_window_max_duration_us: 80_000,
                decoder_random_seek_window_max_duration_us: 0,
            },
            playback_evidence: &evidence,
            process_memory: &memory,
            video_ready_samples: 53_900,
            video_total_samples: 54_000,
            gpu_presented_frames: 54_000,
        });
        assert!(report.passed, "{:?}", report.failures);
    }

    #[test]
    fn rejects_fake_or_incomplete_output_evidence() {
        let mut collector = PlaybackEvidenceCollector::default();
        let mut engine = mondrian_playback::PlaybackEngine::default();
        let time_base = Rational::new(1_001, 30_000);
        engine
            .play_timeline(
                PlaybackTimelineBinding::new(None, 1, time_base, 60_000).expect("binding"),
                FramePosition::new(0, time_base),
                MonotonicTimestamp::ZERO,
            )
            .expect("play");
        engine
            .complete_priming(ClockMaster::AudioDevice, MonotonicTimestamp::ZERO)
            .expect("prime");
        collector
            .observe_snapshot(
                MonotonicTimestamp::ZERO,
                engine.snapshot(),
                engine.frame_demand(),
            )
            .expect("initial evidence");
        collector
            .observe_snapshot(
                MonotonicTimestamp::from_duration(Duration::from_secs(30 * 60)),
                engine.snapshot(),
                engine.frame_demand(),
            )
            .expect("terminal evidence");
        let evidence = collector.report();
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
            qualified_stream_generation: 11,
            audio,
            source_cache: AudioSourceCacheDiagnostics::default(),
            playback_evidence: &evidence,
            process_memory: &memory,
            video_ready_samples: 0,
            video_total_samples: 0,
            gpu_presented_frames: 0,
        });
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"audio_output_snapshot_missing"));
        assert!(codes.contains(&"audio_render_substitution_observed"));
        assert!(codes.contains(&"video_readiness_below_minimum"));
        assert!(codes.contains(&"whole_process_memory_gate_failed"));
        assert!(!report.passed);
    }
}
