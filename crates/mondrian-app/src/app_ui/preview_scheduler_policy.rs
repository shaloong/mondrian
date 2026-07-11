//! Pure deadline and prefetch-window policy for realtime preview scheduling.

use super::preview_access_mode::MediaPreviewRequestPriority;
use mondrian_core::Rational;
use mondrian_media::PreviewDecodeAccessMode;
use std::time::{Duration, Instant};

/// Wall-clock lookahead used to derive playback prefetch depth.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US: u64 = 80_000;
/// Minimum playback prefetch depth for a valid frame rate.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES: usize = 1;
/// Maximum playback prefetch depth regardless of frame rate.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES: usize = 6;

/// Convert an authoritative Frame Demand budget into a worker deadline.
pub(crate) fn media_preview_job_deadline_at(
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    enqueued_at: Instant,
    playback_current_deadline_budget_us: Option<u64>,
) -> Option<Instant> {
    if priority != MediaPreviewRequestPriority::Current
        || access_mode != PreviewDecodeAccessMode::PlaybackCursor
    {
        return None;
    }
    let budget_us = playback_current_deadline_budget_us?;
    enqueued_at.checked_add(Duration::from_micros(budget_us))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_exists_only_for_current_playback() {
        let now = Instant::now();
        assert!(media_preview_job_deadline_at(
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            now,
            Some(40_000),
        )
        .is_some());
        assert_eq!(
            media_preview_job_deadline_at(
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                now,
                Some(40_000),
            ),
            None
        );
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
}
