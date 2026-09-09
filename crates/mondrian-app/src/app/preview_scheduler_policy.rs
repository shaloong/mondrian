//! Pure deadline, execution-quality, and prefetch policy for preview scheduling.

use crate::app::preview_access_mode::{MediaPreviewKey, MediaPreviewRequestPriority};
use std::time::Instant;

use mondrian_core::{types::AssetId, Rational, TimelineTime};
use mondrian_media::{
    PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints, PreviewDecodeDiagnostics,
    PreviewHardwareDecodeDecision, PreviewHardwareDecodeRequest,
};
use mondrian_playback::{
    FrameDeliveryKind, FramePresentationQuality, MAX_BOUNDED_VIDEO_PREROLL_FRAMES,
};

/// Wall-clock lookahead used to derive playback prefetch depth.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US: u64 = 250_000;
/// Minimum playback prefetch depth for a valid frame rate.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES: usize = 1;
/// Maximum playback prefetch depth regardless of frame rate.
///
/// This is only a temporal/work-count guard. Exact Frame Store byte, entry,
/// and decoder-resource-unit headroom remains the physical admission
/// authority. In particular, a native decode is still bounded by its decoder
/// surface-unit grant, while a compact CPU YUV source may use the complete
/// 250 ms horizon when its actual retained plane bytes fit.
pub(crate) const MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES: usize =
    MAX_BOUNDED_VIDEO_PREROLL_FRAMES;
/// Maximum queued plus in-flight prefetch decodes after Priming completes.
///
/// The temporal window and the physical execution queue are separate
/// resources. Keeping at most two executing or queued decodes leaves one
/// native decoder surface available for an exact Current demand when five
/// resident startup/current/cold-source surfaces occupy the Standard grant.
/// This lets the
/// Standard 640 MiB policy retain the complete compact 4K 4:2:2 10-bit
/// lookahead instead of replacing ready frames with reservations for farther
/// work. Priming may still admit the complete bounded prefix before the Clock
/// Master starts.
pub(crate) const MEDIA_PREVIEW_STEADY_PREFETCH_RESERVATION_LIMIT: usize = 2;
/// Consecutive current-frame late results required to declare sustained pressure.
pub(crate) const MEDIA_PREVIEW_PLAYBACK_PRESSURE_LATE_STREAK_THRESHOLD: u64 = 2;
pub(crate) const PREVIEW_SCRUB_HOT_REQUEST_WINDOW_US: u64 = 250_000;
pub(crate) const PREVIEW_SCRUB_SLOW_LATENCY_US: u64 = 40_000;
pub(crate) const PREVIEW_SCRUB_RECOVERY_LATENCY_US: u64 = 25_000;
const PREVIEW_SCRUB_SLOW_SCORE_MAX: u8 = 3;

/// Conservative physical reservation for one speculative decoded frame.
///
/// CPU bytes include both the retained source payload and the lazily
/// materialized working float frame. A renderer-requested compact YUV source
/// has no CPU working fallback, so its exact plane footprint is reserved
/// without inventing a full RGBA float payload. A native-surface identity has
/// no CPU payload or fallback under that same key; it reserves only one
/// decoder-surface unit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MediaPreviewResidencyReservation {
    pub(crate) entries: usize,
    pub(crate) cpu_bytes: usize,
    pub(crate) decoder_resource_units: usize,
}

impl MediaPreviewResidencyReservation {
    fn for_key(key: &MediaPreviewKey, _hardware: PreviewHardwareDecodeRequest) -> Self {
        let resolution = key.residency_resolution();
        let pixels = (resolution.width as usize).saturating_mul(resolution.height as usize);
        let cpu_bytes = if key.decode.representation().is_native_surface() {
            0
        } else if key.decode.representation().is_compact_cpu_yuv() {
            key.decode.source().compact_cpu_yuv_hint().map_or_else(
                || pixels.saturating_mul(2 * 4 * std::mem::size_of::<f32>()),
                |hint| hint.retained_bytes_for_extent(resolution),
            )
        } else {
            // Media owns precision selection for encoded high-depth and linear
            // sources. Every CPU result may also retain working RGBA f32.
            pixels.saturating_mul(
                key.decode.source().cpu_rgba_retained_bytes_per_pixel(key.decode.source_color())
                    + 4 * std::mem::size_of::<f32>(),
            )
        };
        let decoder_resource_units = usize::from(key.decode.representation().is_native_surface());
        Self { entries: 1, cpu_bytes, decoder_resource_units }
    }
}

pub(crate) fn media_preview_residency_reservation(
    key: &MediaPreviewKey,
    hardware: PreviewHardwareDecodeRequest,
) -> MediaPreviewResidencyReservation {
    MediaPreviewResidencyReservation::for_key(key, hardware)
}

/// Structured decode failure consumed by scheduling policy and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewFailureReason {
    Timeout,
    DecodeError,
    ForwardDecodeBudgetExhausted,
    /// An exact Playback or settled still request decoded a different source
    /// timestamp. Only explicitly interactive scrub policy may present a
    /// nearby temporal approximation.
    TemporalMismatch,
    /// The worker contained an unwind from codec, color-materialization, or
    /// frame-construction work and rebuilt its worker-local decode context.
    WorkerPanicked,
    /// A successful decode crossed the Adapter boundary without the physical
    /// residency lease required to retain or present its frame.
    ResidencyContractViolation,
    /// A valid physical lease could not enter the generation's bounded Frame
    /// Store working set.
    ResidencyCapacityRejected,
}

#[derive(Debug, Clone, Copy)]
struct PreviewScrubRequestObservation {
    asset_id: AssetId,
    source_time: TimelineTime,
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
        source_time: TimelineTime,
        observed_at: Instant,
    ) -> PreviewDecodeAdaptiveHints {
        let is_hot_region = self
            .last_request
            .map(|last| {
                last.asset_id == asset_id
                    && observed_at.saturating_duration_since(last.observed_at).as_micros()
                        <= u128::from(PREVIEW_SCRUB_HOT_REQUEST_WINDOW_US)
                    && exact_time_distance(source_time, last.source_time)
                        .is_some_and(source_time_is_within_scrub_hot_window)
            })
            .unwrap_or(false);
        self.hot_request_streak = if is_hot_region {
            self.hot_request_streak.saturating_add(1)
        } else {
            0
        };
        self.last_request =
            Some(PreviewScrubRequestObservation { asset_id, source_time, observed_at });

        let scrub_class = if self.slow_latency_score >= 2 {
            mondrian_media::PreviewScrubAdaptiveClass::SlowLatency
        } else if self.slow_latency_score == 1 {
            mondrian_media::PreviewScrubAdaptiveClass::Recovery
        } else if self.hot_request_streak >= 2 {
            mondrian_media::PreviewScrubAdaptiveClass::HotRegion
        } else {
            mondrian_media::PreviewScrubAdaptiveClass::Normal
        };
        PreviewDecodeAdaptiveHints {
            scrub_class,
            ..PreviewDecodeAdaptiveHints::default()
        }
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

fn exact_time_distance(left: TimelineTime, right: TimelineTime) -> Option<TimelineTime> {
    if left >= right {
        left.checked_sub(right).ok()
    } else {
        right.checked_sub(left).ok()
    }
}

fn source_time_is_within_scrub_hot_window(distance: TimelineTime) -> bool {
    i128::from(distance.numerator()) * 4 <= i128::from(distance.denominator()) * 3
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

/// Bound speculative physical work independently of temporal lookahead depth.
pub(crate) const fn media_preview_steady_prefetch_reservation_limit(window_frames: usize) -> usize {
    if window_frames < MEDIA_PREVIEW_STEADY_PREFETCH_RESERVATION_LIMIT {
        window_frames
    } else {
        MEDIA_PREVIEW_STEADY_PREFETCH_RESERVATION_LIMIT
    }
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

/// Classify temporal presentation quality independently from decode backend.
///
/// Exact software fallback remains `Ready`: backend selection is retained in
/// `PreviewDecodeExecutionSummary`, while presentation quality is reserved for
/// temporal approximation or actual deadline degradation. Conflating the two
/// would make every on-time CPU frame drive Playback recovery and invalidate
/// the very prefetch window needed by a software decoder.
pub(crate) fn preview_decode_presentation_quality(
    diagnostics: &PreviewDecodeDiagnostics,
) -> Result<FramePresentationQuality, MediaPreviewFailureReason> {
    if diagnostics.temporal_approximation {
        if diagnostics.access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return Err(MediaPreviewFailureReason::TemporalMismatch);
        }
        return Ok(FramePresentationQuality::Degraded);
    }
    Ok(FramePresentationQuality::Ready)
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

/// Classify a playback-current worker completion without treating decode
/// backend selection as temporal presentation quality.
///
/// A correct CPU frame after requested-but-unengaged hardware decode remains
/// `Ready`. Actual lateness still drives Playback Quality Policy, while
/// [`playback_hardware_recovery_signals`] retains the independent evidence that
/// proxy or hardware-path recovery may be useful.
pub(crate) fn playback_frame_delivery_kind(
    completed_after_deadline: bool,
    has_frame: bool,
    _priority: MediaPreviewRequestPriority,
    _execution: Option<PlaybackDecodeExecution>,
) -> FrameDeliveryKind {
    if completed_after_deadline {
        return FrameDeliveryKind::Late;
    }
    if !has_frame {
        return FrameDeliveryKind::Failed;
    }
    FrameDeliveryKind::Ready
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
    fn prefetch_window_preserves_the_wall_clock_horizon_at_standard_frame_rates() {
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::new(30, 1)),
            Some(8)
        );
        assert_eq!(
            media_preview_forward_prefetch_window_frames(Rational::new(60_000, 1_001)),
            Some(15)
        );
    }

    fn four_k_key(color_space: mondrian_core::ColorSpace) -> MediaPreviewKey {
        four_k_probed_key(mondrian_core::PixelFormat::Yuv420p, 8, color_space, false)
    }

    fn four_k_compact_yuv_key() -> MediaPreviewKey {
        four_k_probed_key(
            mondrian_core::PixelFormat::Yuv422p10le,
            10,
            mondrian_core::ColorSpace::Rec709,
            true,
        )
    }

    fn four_k_native_surface_key() -> MediaPreviewKey {
        let mut key = four_k_probed_key(
            mondrian_core::PixelFormat::Yuv420p10le,
            10,
            mondrian_core::ColorSpace::Rec2100Pq,
            false,
        );
        key.decode = mondrian_media::PreviewDecodeKey::new(
            key.decode.source().clone(),
            key.decode.source_sample(),
            mondrian_media::PreviewDecodeRepresentation::NativeSurface,
            key.decode.source_color(),
        )
        .expect("probed opaque source permits native output");
        key
    }

    fn four_k_probed_key(
        pixel_format: mondrian_core::PixelFormat,
        bit_depth: u8,
        color_space: mondrian_core::ColorSpace,
        compact: bool,
    ) -> MediaPreviewKey {
        let resolution = mondrian_core::Resolution { width: 3840, height: 2160 };
        let stream = mondrian_media::VideoStreamInfo {
            index: 0,
            codec: mondrian_core::VideoCodec::H264,
            duration: Some(std::time::Duration::from_secs(1)),
            codec_profile: mondrian_media::VideoCodecProfile::H264High422,
            width: resolution.width,
            height: resolution.height,
            picture: mondrian_core::PictureStreamMetadata::default(),
            frame_rate: Rational::new(60_000, 1_001),
            frame_rate_proven: true,
            pixel_format,
            pixel_format_proven: true,
            color_range: mondrian_media::DecodedVideoRange::Limited,
            color_interpretation: mondrian_media::DetectedColorInterpretation::decoder_unavailable(
            ),
            color_metadata: None,
            color_metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
            camera_raw: None,
            bit_depth,
            has_alpha: false,
            avg_bitrate: 205_000_000,
            total_frames: Some(60),
        };
        let source_color = mondrian_media::PreviewSourceColorContract::automatic(
            color_space,
            mondrian_media::DecodedVideoRange::Limited,
        );
        let source = mondrian_media::PreviewDecodeSource::from_probed_stream(
            std::path::PathBuf::from("E:/media/sony-high422.mp4"),
            MediaPreviewKey::test_fingerprint(4_422),
            &stream,
        )
        .expect("valid compact CPU YUV source");
        let representation = if compact {
            mondrian_media::PreviewDecodeRepresentation::canonical(
                &source,
                mondrian_media::PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferHardwareDecode,
                mondrian_media::PreviewRepresentationQuality::Full,
                source_color,
            )
            .expect("valid compact CPU YUV representation")
        } else {
            mondrian_media::PreviewDecodeRepresentation::NativeCpu
        };
        let decode = mondrian_media::PreviewDecodeKey::new(
            source,
            mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
            representation,
            source_color,
        )
        .expect("valid compact CPU YUV key");
        MediaPreviewKey {
            asset_id: AssetId::new(),
            decode,
            source_resolution: resolution,
            picture_geometry: mondrian_core::ResolvedPictureGeometry::square(resolution)
                .expect("valid source geometry"),
            alpha_interpretation: mondrian_core::timeline_data::AlphaInterpretation::Straight,
            preparation_intent: mondrian_renderer::RenderInputTransform::to_working(
                mondrian_core::WorkingColorSpace::LinearRec709,
                false,
                mondrian_core::types::ColorEngine::mondrian_standard(),
            )
            .into(),
        }
    }

    #[test]
    fn four_k_encoded_reservation_covers_source_and_working_payloads() {
        let reservation = media_preview_residency_reservation(
            &four_k_key(mondrian_core::ColorSpace::Rec709),
            PreviewHardwareDecodeRequest::Auto,
        );
        assert_eq!(reservation.entries, 1);
        assert_eq!(
            reservation.cpu_bytes,
            3840usize * 2160usize * (4 + 4 * std::mem::size_of::<f32>())
        );
        assert_eq!(reservation.decoder_resource_units, 0);
    }

    #[test]
    fn encoded_high_depth_reservation_tracks_media_precision_at_every_source_target() {
        for color in [
            mondrian_core::ColorSpace::Rec709,
            mondrian_core::ColorSpace::Rec2100Pq,
        ] {
            let mut key =
                four_k_probed_key(mondrian_core::PixelFormat::Yuv420p10le, 10, color, false);
            // Initial, predictive, seek and cache-recovery re-admissions use
            // the same frozen physical source contract, independently of time.
            for time in [
                TimelineTime::ZERO,
                TimelineTime::new(1, 60).expect("future"),
                TimelineTime::new(17, 24).expect("seek"),
            ] {
                key.decode = mondrian_media::PreviewDecodeKey::new(
                    key.decode.source().clone(),
                    mondrian_core::SourceSampleTarget::covering(time),
                    key.decode.representation(),
                    key.decode.source_color(),
                )
                .expect("same physical source at a new target");
                assert_eq!(
                    media_preview_residency_reservation(&key, PreviewHardwareDecodeRequest::Auto)
                        .cpu_bytes,
                    3840 * 2160 * 32
                );
            }
        }
        let unknown = MediaPreviewKey::test_cpu(
            std::path::PathBuf::from("E:/media/unproven-depth.mov"),
            MediaPreviewKey::test_fingerprint(4_000),
            TimelineTime::ZERO,
            mondrian_core::Resolution { width: 3840, height: 2160 },
            mondrian_media::PreviewSourceColorContract::automatic(
                mondrian_core::ColorSpace::Rec709,
                mondrian_media::DecodedVideoRange::Limited,
            ),
        );
        assert_eq!(
            media_preview_residency_reservation(&unknown, PreviewHardwareDecodeRequest::Auto)
                .cpu_bytes,
            3840 * 2160 * 32
        );
    }

    #[test]
    fn scene_linear_4k_reservation_covers_two_float_payloads() {
        let reservation = media_preview_residency_reservation(
            &four_k_key(mondrian_core::ColorSpace::LinearRec709),
            PreviewHardwareDecodeRequest::Auto,
        );
        assert_eq!(reservation.entries, 1);
        assert_eq!(
            reservation.cpu_bytes,
            3840usize * 2160usize * (2 * 4 * std::mem::size_of::<f32>())
        );
        assert_eq!(reservation.decoder_resource_units, 0);
    }

    #[test]
    fn compact_cpu_yuv_reservation_matches_exact_planes_without_decoder_surface() {
        let key = four_k_compact_yuv_key();
        assert_eq!(
            key.decode.representation(),
            mondrian_media::PreviewDecodeRepresentation::CompactCpuYuv
        );
        let reservation = media_preview_residency_reservation(
            &key,
            PreviewHardwareDecodeRequest::PreferHardwareDecode,
        );
        assert_eq!(reservation.cpu_bytes, 3840usize * 2160usize * 4);
        assert_eq!(reservation.decoder_resource_units, 0);
    }

    #[test]
    fn native_surface_reservation_charges_only_the_decoder_surface() {
        let key = four_k_native_surface_key();
        let reservation = media_preview_residency_reservation(
            &key,
            PreviewHardwareDecodeRequest::PreferGpuResident,
        );
        assert_eq!(reservation.entries, 1);
        assert_eq!(reservation.cpu_bytes, 0);
        assert_eq!(reservation.decoder_resource_units, 1);
    }

    #[test]
    fn requested_but_unengaged_hardware_decode_is_temporally_ready() {
        assert_eq!(
            playback_frame_delivery_kind(
                false,
                true,
                MediaPreviewRequestPriority::Current,
                Some(software_fallback_execution()),
            ),
            FrameDeliveryKind::Ready
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
            adaptation.observe_request(asset_id, TimelineTime::ZERO, started_at).scrub_class,
            mondrian_media::PreviewScrubAdaptiveClass::Normal
        );
        assert_eq!(
            adaptation
                .observe_request(
                    asset_id,
                    TimelineTime::new(1, 10).expect("exact source time"),
                    started_at + std::time::Duration::from_millis(1)
                )
                .scrub_class,
            mondrian_media::PreviewScrubAdaptiveClass::Normal
        );
        assert_eq!(
            adaptation
                .observe_request(
                    asset_id,
                    TimelineTime::new(1, 5).expect("exact source time"),
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
                    TimelineTime::new(3, 10).expect("exact source time"),
                    started_at + std::time::Duration::from_millis(3)
                )
                .scrub_class,
            mondrian_media::PreviewScrubAdaptiveClass::SlowLatency
        );
    }
}
