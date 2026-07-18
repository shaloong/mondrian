//! Pure deadline, execution-quality, and prefetch policy for preview scheduling.

use crate::app::preview_access_mode::MediaPreviewRequestPriority;
use std::time::Instant;

use mondrian_core::{types::AssetId, Rational};
use mondrian_media::{
    PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints, PreviewDecodeDiagnostics,
    PreviewHardwareDecodeDecision, PreviewHardwareDecodeRequest,
};
use mondrian_playback::{FrameDeliveryKind, FramePresentationQuality};

/// Wall-clock lookahead used to derive playback prefetch depth.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US: u64 = 250_000;
/// Minimum playback prefetch depth for a valid frame rate.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES: usize = 1;
/// Maximum playback prefetch depth regardless of frame rate.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES: usize =
    mondrian_playback::MAX_BOUNDED_VIDEO_PREROLL_FRAMES;
/// Consecutive current-frame late results required to declare sustained pressure.
pub(crate) const MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD: u64 = 2;
pub(crate) const PREVIEW_SCRUB_HOT_REQUEST_WINDOW_US: u64 = 250_000;
pub(crate) const PREVIEW_SCRUB_HOT_SOURCE_WINDOW_US: i64 = 750_000;
pub(crate) const PREVIEW_SCRUB_SLOW_LATENCY_US: u64 = 40_000;
pub(crate) const PREVIEW_SCRUB_RECOVERY_LATENCY_US: u64 = 25_000;
const PREVIEW_SCRUB_SLOW_SCORE_MAX: u8 = 3;

/// Structured decode failure consumed by scheduling policy and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewFailureReason {
    Timeout,
    DecodeError,
    ForwardDecodeBudgetExhausted,
}

#[derive(Debug, Clone, Copy)]
struct PreviewScrubRequestObservation {
    asset_id: AssetId,
    source_micros: i64,
    observed_at: Instant,
}

/// UI-independent scrub locality and latency adaptation state.
#[derive(Debug, Default)]
pub(crate) struct PreviewScrubAdaptationState {
    last_request: Option<PreviewScrubRequestObservation>,
    hot_request_streak: u8,
    slow_latency_score: u8,
}

impl PreviewScrubAdaptationState {
    /// Observe an explicitly timestamped scrub request and return decoder hints.
    pub(crate) fn observe_request(
        &mut self,
        asset_id: AssetId,
        source_micros: i64,
        observed_at: Instant,
    ) -> PreviewDecodeAdaptiveHints {
        let is_hot_region = self
            .last_request
            .map(|last| {
                last.asset_id == asset_id
                    && observed_at.saturating_duration_since(last.observed_at).as_micros()
                        <= u128::from(PREVIEW_SCRUB_HOT_REQUEST_WINDOW_US)
                    && source_micros.saturating_sub(last.source_micros).abs()
                        <= PREVIEW_SCRUB_HOT_SOURCE_WINDOW_US
            })
            .unwrap_or(false);
        self.hot_request_streak = if is_hot_region {
            self.hot_request_streak.saturating_add(1)
        } else {
            0
        };
        self.last_request =
            Some(PreviewScrubRequestObservation { asset_id, source_micros, observed_at });

        let scrub_class = if self.slow_latency_score >= 2 {
            mondrian_media::PreviewScrubAdaptiveClass::SlowLatency
        } else if self.slow_latency_score == 1 {
            mondrian_media::PreviewScrubAdaptiveClass::Recovery
        } else if self.hot_request_streak >= 2 {
            mondrian_media::PreviewScrubAdaptiveClass::HotRegion
        } else {
            mondrian_media::PreviewScrubAdaptiveClass::Normal
        };
        PreviewDecodeAdaptiveHints { scrub_class }
    }

    /// Observe frame-local decode latency for a completed scrub request.
    pub(crate) fn observe_decode(&mut self, diagnostics: PreviewDecodeDiagnostics) {
        if diagnostics.access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return;
        }
        if diagnostics.elapsed_us >= PREVIEW_SCRUB_SLOW_LATENCY_US {
            self.slow_latency_score =
                self.slow_latency_score.saturating_add(1).min(PREVIEW_SCRUB_SLOW_SCORE_MAX);
        } else if diagnostics.elapsed_us <= PREVIEW_SCRUB_RECOVERY_LATENCY_US {
            self.slow_latency_score = self.slow_latency_score.saturating_sub(1);
        }
    }

    /// Observe a decode failure that proves the current scrub strategy is too slow.
    pub(crate) fn observe_failure(
        &mut self,
        access_mode: PreviewDecodeAccessMode,
        reason: MediaPreviewFailureReason,
    ) {
        if access_mode == PreviewDecodeAccessMode::ScrubCursor
            && reason == MediaPreviewFailureReason::ForwardDecodeBudgetExhausted
        {
            self.slow_latency_score = self.slow_latency_score.max(2);
        }
    }
}

/// Edge emitted when playback pressure changes acceptance state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackPressureTransition {
    Unchanged,
    Entered,
    Recovered,
}

/// UI-independent consecutive-late state used by decode and prefetch policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlaybackPressureState {
    late_streak: u64,
}

impl PlaybackPressureState {
    /// Observe one or more late current-frame outcomes.
    pub(crate) fn observe_late(&mut self, count: u64) -> PlaybackPressureTransition {
        if count == 0 {
            return PlaybackPressureTransition::Unchanged;
        }
        let was_active = self.is_active();
        self.late_streak = self.late_streak.saturating_add(count);
        if !was_active && self.is_active() {
            PlaybackPressureTransition::Entered
        } else {
            PlaybackPressureTransition::Unchanged
        }
    }

    /// Observe a successful decode result, resetting only playback-current pressure.
    pub(crate) fn observe_success(
        &mut self,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) -> PlaybackPressureTransition {
        if priority != MediaPreviewRequestPriority::Current
            || access_mode != PreviewDecodeAccessMode::PlaybackCursor
        {
            return PlaybackPressureTransition::Unchanged;
        }
        let was_active = self.is_active();
        self.late_streak = 0;
        if was_active {
            PlaybackPressureTransition::Recovered
        } else {
            PlaybackPressureTransition::Unchanged
        }
    }

    /// Consecutive late current-frame count.
    pub(crate) const fn late_streak(self) -> u64 {
        self.late_streak
    }

    /// Whether sustained playback pressure is active.
    pub(crate) const fn is_active(self) -> bool {
        self.late_streak >= MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD
    }
}

/// Derive bounded prefetch depth from a wall-clock horizon and exact frame rate.
pub(crate) fn media_preview_forward_prefetch_window_frames(frame_rate: Rational) -> Option<usize> {
    let fps = frame_rate.to_f64();
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    let frames = ((MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US as f64 / 1_000_000.0) * fps).ceil();
    if !frames.is_finite() {
        return None;
    }
    Some((frames.max(1.0) as usize).clamp(
        MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
        MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
    ))
}

/// Whether the request selected a hardware-decode preference rather than CPU-only Auto.
pub(crate) fn playback_hardware_decode_requested(request: PreviewHardwareDecodeRequest) -> bool {
    matches!(
        request,
        PreviewHardwareDecodeRequest::PreferHardwareDecode
            | PreviewHardwareDecodeRequest::PreferGpuResident
            | PreviewHardwareDecodeRequest::RequireGpuResident
    )
}

/// Whether real decode evidence proves hardware execution for this frame.
pub(crate) fn preview_hardware_decode_effective(diagnostics: &PreviewDecodeDiagnostics) -> bool {
    diagnostics.hardware_decode_decision == PreviewHardwareDecodeDecision::GpuResidentNative
        || diagnostics.hardware_decode_cpu_transfer_observed
}

/// Preserve executed decode quality on the produced frame so prefetch/cache
/// reuse cannot lose hardware-fallback evidence.
pub(crate) fn preview_decode_presentation_quality(
    diagnostics: &PreviewDecodeDiagnostics,
) -> FramePresentationQuality {
    if diagnostics.temporal_approximation
        || (playback_hardware_decode_requested(diagnostics.hardware_decode_request)
            && !preview_hardware_decode_effective(diagnostics))
    {
        FramePresentationQuality::Degraded
    } else {
        FramePresentationQuality::Ready
    }
}

/// Minimal executed decode facts consumed by playback delivery policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackDecodeExecution {
    pub(crate) access_mode: PreviewDecodeAccessMode,
    pub(crate) hardware_decode_request: PreviewHardwareDecodeRequest,
    pub(crate) hardware_decode_effective: bool,
}

impl From<&PreviewDecodeDiagnostics> for PlaybackDecodeExecution {
    fn from(diagnostics: &PreviewDecodeDiagnostics) -> Self {
        Self {
            access_mode: diagnostics.access_mode,
            hardware_decode_request: diagnostics.hardware_decode_request,
            hardware_decode_effective: preview_hardware_decode_effective(diagnostics),
        }
    }
}

/// Hardware-path recovery facts derived without UI diagnostics or counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlaybackHardwareRecoverySignals {
    pub(crate) native_import_unavailable: bool,
    pub(crate) hardware_fallback_not_engaged: bool,
}

impl PlaybackHardwareRecoverySignals {
    /// Whether either signal recommends proxy or hardware-path recovery.
    pub(crate) const fn recovery_recommended(self) -> bool {
        self.native_import_unavailable || self.hardware_fallback_not_engaged
    }
}

/// Derive playback-current hardware recovery signals from configuration and
/// frame-local execution provenance.
pub(crate) fn playback_hardware_recovery_signals(
    priority: MediaPreviewRequestPriority,
    configured_request: PreviewHardwareDecodeRequest,
    native_import_admission_ready: bool,
    execution: PlaybackDecodeExecution,
) -> PlaybackHardwareRecoverySignals {
    if priority != MediaPreviewRequestPriority::Current
        || execution.access_mode != PreviewDecodeAccessMode::PlaybackCursor
    {
        return PlaybackHardwareRecoverySignals::default();
    }
    PlaybackHardwareRecoverySignals {
        native_import_unavailable: playback_hardware_decode_requested(configured_request)
            && !native_import_admission_ready,
        hardware_fallback_not_engaged: playback_hardware_decode_requested(
            execution.hardware_decode_request,
        ) && !execution.hardware_decode_effective,
    }
}

/// Classify a playback-current worker completion without treating capability
/// probes as execution. A correct CPU frame after requested-but-unengaged
/// hardware decode is presentable, but explicitly Degraded so Playback Quality
/// Policy can lower temporary resolution.
pub(crate) fn playback_frame_delivery_kind(
    completed_after_deadline: bool,
    has_frame: bool,
    priority: MediaPreviewRequestPriority,
    execution: Option<PlaybackDecodeExecution>,
) -> FrameDeliveryKind {
    if completed_after_deadline {
        return FrameDeliveryKind::Late;
    }
    if !has_frame {
        return FrameDeliveryKind::Failed;
    }
    let hardware_fallback = priority == MediaPreviewRequestPriority::Current
        && execution.is_some_and(|execution| {
            execution.access_mode == PreviewDecodeAccessMode::PlaybackCursor
                && playback_hardware_decode_requested(execution.hardware_decode_request)
                && !execution.hardware_decode_effective
        });
    if hardware_fallback {
        FrameDeliveryKind::Degraded
    } else {
        FrameDeliveryKind::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn software_fallback_execution() -> PlaybackDecodeExecution {
        PlaybackDecodeExecution {
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
            hardware_decode_request: PreviewHardwareDecodeRequest::PreferHardwareDecode,
            hardware_decode_effective: false,
        }
    }

    #[test]
    fn prefetch_window_is_bounded_for_extreme_frame_rates() {
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::new(240, 1)),
            Some(MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::new(1, 1)),
            Some(MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES)
        );
    }

    #[test]
    fn requested_but_unengaged_hardware_decode_is_a_degraded_delivery() {
        assert_eq!(
            playback_frame_delivery_kind(
                false,
                true,
                MediaPreviewRequestPriority::Current,
                Some(software_fallback_execution()),
            ),
            FrameDeliveryKind::Degraded
        );
    }

    #[test]
    fn actual_hardware_cpu_transfer_is_ready_not_degraded() {
        let mut execution = software_fallback_execution();
        execution.hardware_decode_effective = true;

        assert_eq!(
            playback_frame_delivery_kind(
                false,
                true,
                MediaPreviewRequestPriority::Current,
                Some(execution),
            ),
            FrameDeliveryKind::Ready
        );
    }

    #[test]
    fn hardware_recovery_signals_require_playback_current_execution() {
        let execution = software_fallback_execution();
        let signals = playback_hardware_recovery_signals(
            MediaPreviewRequestPriority::Current,
            PreviewHardwareDecodeRequest::PreferGpuResident,
            false,
            execution,
        );
        assert!(signals.native_import_unavailable);
        assert!(signals.hardware_fallback_not_engaged);
        assert!(signals.recovery_recommended());

        assert_eq!(
            playback_hardware_recovery_signals(
                MediaPreviewRequestPriority::Prefetch,
                PreviewHardwareDecodeRequest::PreferGpuResident,
                false,
                execution,
            ),
            PlaybackHardwareRecoverySignals::default()
        );
    }

    #[test]
    fn playback_pressure_enters_once_and_recovers_only_on_current_success() {
        let mut pressure = PlaybackPressureState::default();
        assert_eq!(
            pressure.observe_late(1),
            PlaybackPressureTransition::Unchanged
        );
        assert!(!pressure.is_active());
        assert_eq!(
            pressure.observe_late(1),
            PlaybackPressureTransition::Entered
        );
        assert!(pressure.is_active());
        assert_eq!(
            pressure.observe_late(5),
            PlaybackPressureTransition::Unchanged
        );
        assert_eq!(pressure.late_streak(), 7);
        assert_eq!(
            pressure.observe_success(
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            PlaybackPressureTransition::Unchanged
        );
        assert!(pressure.is_active());
        assert_eq!(
            pressure.observe_success(
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
            ),
            PlaybackPressureTransition::Recovered
        );
        assert_eq!(pressure.late_streak(), 0);
    }

    #[test]
    fn scrub_adaptation_uses_explicit_time_locality_and_failure_evidence() {
        let asset_id = AssetId::new();
        let started_at = Instant::now();
        let mut adaptation = PreviewScrubAdaptationState::default();

        assert_eq!(
            adaptation.observe_request(asset_id, 0, started_at).scrub_class,
            mondrian_media::PreviewScrubAdaptiveClass::Normal
        );
        assert_eq!(
            adaptation
                .observe_request(
                    asset_id,
                    100_000,
                    started_at + std::time::Duration::from_millis(1)
                )
                .scrub_class,
            mondrian_media::PreviewScrubAdaptiveClass::Normal
        );
        assert_eq!(
            adaptation
                .observe_request(
                    asset_id,
                    200_000,
                    started_at + std::time::Duration::from_millis(2)
                )
                .scrub_class,
            mondrian_media::PreviewScrubAdaptiveClass::HotRegion
        );

        adaptation.observe_failure(
            PreviewDecodeAccessMode::ScrubCursor,
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted,
        );
        assert_eq!(
            adaptation
                .observe_request(
                    asset_id,
                    300_000,
                    started_at + std::time::Duration::from_millis(3)
                )
                .scrub_class,
            mondrian_media::PreviewScrubAdaptiveClass::SlowLatency
        );
    }
}
