//! Pure deadline, execution-quality, and prefetch policy for preview scheduling.

use crate::app::preview_access_mode::MediaPreviewRequestPriority;
use mondrian_core::Rational;
use mondrian_media::{
    PreviewDecodeAccessMode, PreviewDecodeDiagnostics, PreviewHardwareDecodeDecision,
    PreviewHardwareDecodeRequest,
};
use mondrian_playback::{FrameDeliveryKind, FramePresentationQuality};

/// Wall-clock lookahead used to derive playback prefetch depth.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US: u64 = 80_000;
/// Minimum playback prefetch depth for a valid frame rate.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES: usize = 1;
/// Maximum playback prefetch depth regardless of frame rate.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES: usize = 6;
/// Consecutive current-frame late results required to declare sustained pressure.
pub(crate) const MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD: u64 = 2;

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
    let frames = ((MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US as f64 / 1_000_000.0) * fps).round();
    if !frames.is_finite() || frames <= 0.0 {
        return None;
    }
    Some((frames as usize).clamp(
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
            media_preview_forward_prefetch_window_frames(Rational::FPS_10),
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
}
