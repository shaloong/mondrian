//! Generated-picture Golden slice: author, export, validate, and reimport.

use super::fixture::{resolve_fixture, sha256_bytes, sha256_file, CorpusManifest, FixtureEvidence};
use super::harness::{
    author_transition, dispatch_author_transition, ensure_exact_requirement_evidence,
    execute_export_job, fixture_root, wait_for_media_imports, AuthorTransitionEvidence,
    DurableReopenEvidence,
};
use super::media_execution::{decode_media, rgba8_at, source_rgba};
use super::workflow::{GoldenProductWorkflowDriver, GoldenSequenceStageEvidence};
use super::{
    builtin_preset, load_json, sequence_settings_from_contract, GoldenExportContract,
    GoldenProjectContract,
};
use crate::app::audio_rendering::TimelineAudioPcmRenderer;
use crate::app::preview_cpu_execution::composite_resolved_preview_working;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline, PreviewTimelineMediaFrame, PreviewTimelineMediaRequest,
    PreviewTimelineResolution, PreviewTimelineTitleFrame,
};
use crate::app::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use crate::app::preview_viewer_plan::ResolvedPreviewElement;
use crate::app::ui_actions::{
    assets_prepare_drag_action, inspector_set_clip_opacity_action,
    inspector_set_clip_transform_field_action, timeline_trim_clips_action,
    AssetsPrepareDragPayload, InspectorClipRefPayload, InspectorClipTransformField,
    InspectorSetClipOpacityPayload, InspectorSetClipTransformFieldPayload,
    TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
};
use crate::app::{AppState, ClipOverlapMode};
use anyhow::{bail, ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::{
    timeline_data::AlphaInterpretation, AssetId, AudioChannelLayout, AudioSamplePosition,
    AudioSampleRate, AudioSampleRounding, BlendMode, ClipId, ExecutionCancellationToken,
    ExecutionTerminalDisposition, FramePosition, Resolution, SequenceId, TimelineTime, TrackId,
};
use mondrian_editor_state::Action;
use mondrian_export::preset::TimelineExportRange;
use mondrian_export::queue::ExportJobDiagnostics;
use mondrian_export::validator::{probe_export_output, ExportOutputProbe, ProbedStreamTiming};
use mondrian_media::info::{AudioCodec, ChannelLayout, PixelFormat, VideoCodec, VideoCodecProfile};
use mondrian_media::{
    AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderRequest, AudioPcmRenderer,
    PreviewDecodeSessionContext,
};
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    execute_cpu_output_boundary, CpuColorFrame, RenderOutputColorBoundary,
    RenderOutputColorBoundaryTarget, TimelineCompositeScratch,
};
use mondrian_timeline::sequence::InputColorResolutionSource;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) const DELIVERY_SLICE_ID: &str = "generated-delivery-roundtrip-v1";
const EXPORT_TIMEOUT: Duration = Duration::from_secs(600);
const VISUAL_ROUNDTRIP_MAX_RGB_ERROR: u8 = 16;
const VISUAL_ORACLE_MAX_RGB_ERROR: u8 = 4;
const AUDIO_MIN_RMS: f64 = 0.005;
const AUDIO_MIN_PEAK: f32 = 0.01;
const AUDIO_MIN_CHANNEL_RMS_DELTA: f64 = 0.002;
const AUDIO_ROUNDTRIP_MAX_RMS_ABSOLUTE_ERROR: f64 = 0.01;
const AUDIO_ROUNDTRIP_MAX_RMS_RELATIVE_ERROR: f64 = 0.25;
const AUDIO_ROUNDTRIP_MAX_PEAK_ABSOLUTE_ERROR: f32 = 0.04;
const AUDIO_CODEC_AGREEMENT_MAX_RMS_DELTA: f64 = 0.004;

#[derive(Debug, Serialize)]
pub(super) struct GoldenDeliveryReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    corpus_revision: String,
    status: &'static str,
    complete_golden_project: bool,
    fixture: FixtureEvidence,
    setup: DeliverySetupEvidence,
    operations: Vec<OperationEvidence>,
    content: Vec<ContentEvidence>,
    authoring: GoldenDeliveryAuthoringAnchor,
}

#[derive(Debug, Serialize)]
struct DeliverySetupEvidence {
    project_path: PathBuf,
    stage: GoldenSequenceStageEvidence,
    video_track_id: TrackId,
    pcm_audio_track_id: TrackId,
    solid_asset_id: AssetId,
    pcm_audio_asset_id: AssetId,
    solid_clip_id: ClipId,
    pcm_audio_clip_id: ClipId,
    asset_library_revision_before_solid: u64,
    asset_library_revision_after_solid: u64,
    place_solid: AuthorTransitionEvidence,
    durable_reopen: DurableReopenEvidence,
    start_frame: i64,
    end_frame_exclusive: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GoldenDeliveryAuthoringAnchor {
    sequence_id: SequenceId,
    video_track_id: TrackId,
    solid_asset_id: AssetId,
    solid_clip_id: ClipId,
    track_sha256: String,
    start: TimelineTime,
    duration: TimelineTime,
    clip_time_in: TimelineTime,
    position_x: f32,
    position_y: f32,
    scale_x: f32,
    scale_y: f32,
    opacity: f32,
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum OperationEvidence {
    Trim {
        author_step: AuthorTransitionEvidence,
        clip_ids: Vec<ClipId>,
        end_frame_exclusive: i64,
    },
    Export {
        deliveries: Vec<ExportEvidence>,
    },
    Reimport {
        assets: Vec<ReimportEvidence>,
        audio_codec_agreement: AudioCodecAgreementEvidence,
    },
}

impl OperationEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::Trim { .. } => "trim",
            Self::Export { .. } => "export",
            Self::Reimport { .. } => "reimport",
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ExportEvidence {
    export_id: String,
    job_id: mondrian_core::JobId,
    generation: u64,
    executed: bool,
    terminal_disposition: ExecutionTerminalDisposition,
    diagnostics: ExportJobDiagnostics,
    output_path: PathBuf,
    output_sha256: String,
    probe: ExportOutputProbe,
    av_boundaries: AvBoundaryEvidence,
}

impl ExportEvidence {
    pub(super) fn output_path(&self) -> &Path {
        &self.output_path
    }
}

#[derive(Debug, Serialize)]
struct AvBoundaryEvidence {
    allowed_error_ms: u32,
    video_start_offset_ms: f64,
    audio_start_offset_ms: f64,
    start_alignment_error_ms: f64,
    video_end_error_ms: f64,
    audio_end_error_ms: f64,
    end_alignment_error_ms: f64,
}

#[derive(Debug, Serialize)]
struct ReimportEvidence {
    export_id: String,
    asset_id: AssetId,
    container: String,
    video_codec: VideoCodec,
    video_profile: VideoCodecProfile,
    width: u32,
    height: u32,
    frame_rate: mondrian_core::Rational,
    pixel_format: PixelFormat,
    pixel_format_proven: bool,
    bit_depth: u8,
    audio_codec: AudioCodec,
    audio_sample_rate: u32,
    audio_channels: u8,
    audio_channel_layout: ChannelLayout,
    input_color_resolution: InputColorResolutionSource,
    audio_roundtrip: AudioRoundtripEvidence,
    visual_roundtrip: VisualRoundtripEvidence,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct AudioSignalEvidence {
    start_sample: i64,
    frame_count: usize,
    left_rms: f64,
    right_rms: f64,
    left_peak: f32,
    right_peak: f32,
}

#[derive(Debug, Serialize)]
struct AudioRoundtripEvidence {
    reference: AudioSignalEvidence,
    decoded: AudioSignalEvidence,
    left_rms_absolute_error: f64,
    right_rms_absolute_error: f64,
    max_rms_relative_error: f64,
    max_peak_absolute_error: f32,
    allowed_rms_absolute_error: f64,
    allowed_rms_relative_error: f64,
    allowed_peak_absolute_error: f32,
}

#[derive(Debug, Serialize)]
struct AudioCodecAgreementEvidence {
    left_rms_delta: f64,
    right_rms_delta: f64,
    allowed_rms_delta: f64,
}

#[derive(Debug, Serialize)]
struct VisualRoundtripEvidence {
    width: u32,
    height: u32,
    reference_rgba_sha256: String,
    decoded_rgba_sha256: String,
    sampled_pixels: Vec<VisualSampleEvidence>,
    max_channel_error: u8,
    allowed_max_channel_error: u8,
    independent_opacity_oracle_max_error: u8,
    allowed_opacity_oracle_max_error: u8,
    transformed_inside_outside_proven: bool,
}

#[derive(Debug, Serialize)]
struct VisualSampleEvidence {
    role: &'static str,
    x: u32,
    y: u32,
    reference: [u8; 4],
    decoded: [u8; 4],
    max_rgb_error: u8,
}

struct ProgramReference {
    resolution: Resolution,
    rgba: Vec<u8>,
    rgba_sha256: String,
    independent_opacity_oracle_max_error: u8,
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum ContentEvidence {
    Transform {
        author_steps: Vec<AuthorTransitionEvidence>,
        position_x: f32,
        position_y: f32,
        scale_x: f32,
        scale_y: f32,
    },
    Opacity {
        author_step: AuthorTransitionEvidence,
        value: f32,
    },
}

impl ContentEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::Transform { .. } => "transform",
            Self::Opacity { .. } => "opacity",
        }
    }
}

impl GoldenDeliveryReport {
    pub(super) fn primary_sequence_id(&self) -> SequenceId {
        self.setup.stage.sequence_id()
    }

    pub(super) fn verify_retained_authoring(&self, state: &AppState) -> anyhow::Result<()> {
        ensure!(
            capture_delivery_authoring_anchor(
                state,
                self.primary_sequence_id(),
                self.setup.video_track_id,
                self.setup.solid_clip_id,
                self.setup.solid_asset_id,
            )? == self.authoring,
            "Delivery Track-owned visual authoring changed after the stage"
        );
        Ok(())
    }
}

fn capture_delivery_authoring_anchor(
    state: &AppState,
    sequence_id: SequenceId,
    video_track_id: TrackId,
    solid_clip_id: ClipId,
    solid_asset_id: AssetId,
) -> anyhow::Result<GoldenDeliveryAuthoringAnchor> {
    let sequence = state.sequence_by_id(sequence_id).context("Delivery Hero Sequence is absent")?;
    let track = sequence
        .video_tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .context("Delivery video Track is absent")?;
    ensure!(
        track.clips.len() == 1,
        "Delivery video Track no longer contains exactly one owned Clip"
    );
    let clip = track
        .clips
        .iter()
        .find(|clip| clip.id == solid_clip_id)
        .context("Delivery solid Clip is absent")?;
    ensure!(
        clip.is_solid_color() && clip.library_asset_id() == Some(solid_asset_id),
        "Delivery Clip changed content or Asset identity"
    );
    let author_time = clip.clip_time_in;
    let position = clip.transform.get_position(author_time);
    let scale = clip.transform.get_scale(author_time);
    Ok(GoldenDeliveryAuthoringAnchor {
        sequence_id,
        video_track_id,
        solid_asset_id,
        solid_clip_id,
        track_sha256: sha256_bytes(&serde_json::to_vec(track)?),
        start: clip.position,
        duration: clip.duration,
        clip_time_in: clip.clip_time_in,
        position_x: position.x,
        position_y: position.y,
        scale_x: scale.x,
        scale_y: scale.y,
        opacity: clip.transform.evaluate_opacity(author_time),
    })
}

fn normalized_identity(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn expect_probe_string(field: &str, actual: Option<&str>, expected: &str) -> anyhow::Result<()> {
    ensure!(
        actual == Some(expected),
        "{field} mismatch: expected {expected}, actual {}",
        actual.unwrap_or("<missing>")
    );
    Ok(())
}

fn assert_probe_matches_contract(
    probe: &ExportOutputProbe,
    export: &GoldenExportContract,
    frame_rate: mondrian_core::Rational,
    expected_duration_frames: i64,
    duration_error_max_frames: u32,
    audio_sample_rate: u32,
) -> anyhow::Result<()> {
    let expected = &export.expected_delivery;
    let container_format =
        probe.container_format.as_deref().context("container format is unproven")?;
    ensure!(
        expected.container == "mp4"
            && container_format.split(',').any(|identity| identity.trim() == "mp4")
            && probe
                .container_major_brand
                .as_deref()
                .is_some_and(|brand| !brand.eq_ignore_ascii_case("qt")),
        "mux/container identity differs from the Golden delivery contract"
    );
    let video = probe.video.as_ref().context("validated export probe has no video stream")?;
    expect_probe_string(
        "video codec",
        video.codec_name.as_deref(),
        &expected.video_codec,
    )?;
    let actual_profile = video.profile.as_deref().context("video profile is unproven")?;
    ensure!(
        normalized_identity(actual_profile) == normalized_identity(&expected.video_profile),
        "video profile mismatch: expected {}, actual {actual_profile}",
        expected.video_profile
    );
    ensure!(
        video.width == Some(expected.width) && video.height == Some(expected.height),
        "encoded dimensions differ from Golden delivery contract"
    );
    ensure!(
        video.frame_rate_num == Some(frame_rate.num)
            && video.frame_rate_den == Some(frame_rate.den),
        "encoded frame rate differs from Golden timeline"
    );
    ensure!(
        video.bit_depth == Some(expected.bit_depth),
        "encoded bit depth differs from Golden delivery contract"
    );
    expect_probe_string(
        "pixel format",
        video.pixel_format.as_deref(),
        &expected.pixel_format,
    )?;
    expect_probe_string(
        "color primaries",
        video.color_primaries.as_deref(),
        &expected.color_primaries,
    )?;
    expect_probe_string(
        "color transfer",
        video.color_transfer.as_deref(),
        &expected.color_transfer,
    )?;
    expect_probe_string(
        "color matrix",
        video.color_matrix.as_deref(),
        &expected.color_matrix,
    )?;
    let expected_range = match expected.range.as_str() {
        "legal" => "tv",
        "full" => "pc",
        other => anyhow::bail!("unsupported Golden range identity: {other}"),
    };
    expect_probe_string("video range", video.color_range.as_deref(), expected_range)?;
    ensure!(
        expected.static_hdr_metadata == "absent"
            && !video.mastering_display_metadata_present
            && !video.content_light_metadata_present,
        "Golden SDR delivery unexpectedly contains static HDR metadata"
    );
    let actual_duration = probe.duration_secs.context("export duration is unproven")?;
    let expected_duration =
        expected_duration_frames as f64 * frame_rate.den as f64 / frame_rate.num as f64;
    let duration_error_frames = (actual_duration - expected_duration).abs() * frame_rate.to_f64();
    ensure!(
        duration_error_frames <= f64::from(duration_error_max_frames),
        "export duration error is {duration_error_frames:.3} frames"
    );
    let audio = probe.audio.as_ref().context("validated export probe has no audio stream")?;
    expect_probe_string(
        "audio codec",
        audio.codec_name.as_deref(),
        &expected.audio_codec,
    )?;
    ensure!(
        audio.sample_rate == Some(audio_sample_rate),
        "encoded audio sample rate differs from Golden timeline"
    );
    ensure!(
        audio.channels == Some(2) && audio.channel_layout.as_deref() == Some("stereo"),
        "encoded audio layout differs from Golden timeline"
    );
    Ok(())
}

fn rec709_code(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear < 0.018 {
        4.5 * linear
    } else {
        1.099 * linear.powf(0.45) - 0.099
    };
    (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn visual_sample_coordinates(resolution: Resolution) -> [(&'static str, u32, u32); 4] {
    [
        ("center", resolution.width / 2, resolution.height / 2),
        ("left-inside", resolution.width / 10, resolution.height / 2),
        ("left-outside", resolution.width / 50, resolution.height / 2),
        ("top-outside", resolution.width / 2, resolution.height / 50),
    ]
}

fn max_rgb_error(left: [u8; 4], right: [u8; 4]) -> u8 {
    (0..3).map(|channel| left[channel].abs_diff(right[channel])).max().unwrap_or(0)
}

fn render_program_reference(
    state: &AppState,
    frame: i64,
    resolution: Resolution,
    solid_clip_id: ClipId,
) -> anyhow::Result<ProgramReference> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let solid_clip = sequence
        .video_tracks
        .iter()
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == solid_clip_id)
        .context("delivery solid Clip is absent for Program reference")?;
    let solid_color =
        solid_clip.content.solid_color().context("delivery Clip is not Solid Color")?;
    let clip_time = solid_clip.timeline_to_clip_time(TimelineTime::from_frame_position(
        FramePosition::new(frame, sequence.time_base()),
    )?)?;
    let opacity = solid_clip.transform.evaluate_opacity(clip_time);

    let mut media_frame = |_request| PreviewTimelineMediaFrame::Unavailable {
        reason: PreviewUnavailability::blocked(
            PreviewOutputStage::MediaResolution,
            "delivery work area contains no video media",
        ),
    };
    let mut title_frame = |_request| PreviewTimelineTitleFrame::Unavailable {
        reason: PreviewUnavailability::blocked(
            PreviewOutputStage::GeneratedSource,
            "delivery work area contains no generated title",
        ),
    };
    let color_context =
        sequence.settings.root_program_color_context(state.project_color_environment());
    let resolved = match resolve_preview_timeline(
        sequence,
        state.sequences(),
        frame,
        resolution,
        PreviewResolutionScale::Full,
        color_context,
        &mut media_frame,
        &mut title_frame,
    ) {
        PreviewTimelineResolution::Ready(resolved) => resolved,
        PreviewTimelineResolution::Empty => bail!("delivery Program reference resolved as empty"),
        PreviewTimelineResolution::Pending { .. } => {
            bail!("delivery Program reference retained a pending dependency")
        }
        PreviewTimelineResolution::Unavailable { reason } => {
            bail!("delivery Program reference unavailable: {reason:?}")
        }
    };
    ensure!(
        resolved.plan.elements.len() == 1
            && matches!(
                resolved.plan.elements.first(),
                Some(ResolvedPreviewElement::SolidColor(_))
            ),
        "delivery work area did not resolve exactly one Solid Color layer"
    );
    let mut scratch = TimelineCompositeScratch::default();
    let working = composite_resolved_preview_working(
        resolution.width,
        resolution.height,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        &mut scratch,
    )?;
    ensure!(
        working.composite_diagnostics.float_linear_composites == 1
            && working.composite_diagnostics.legacy_rgba8_composites == 0
            && !working.composite_diagnostics.is_color_domain_blocked(),
        "delivery Program reference left the working-linear path"
    );
    let mut flattened = working.frame.into_rgba_f32();
    for pixel in &mut flattened.data {
        let coverage = pixel[3].clamp(0.0, 1.0);
        pixel[0] *= coverage;
        pixel[1] *= coverage;
        pixel[2] *= coverage;
        pixel[3] = 1.0;
    }
    let flattened = CpuColorFrame::working(flattened);
    let program_output_color = resolved
        .plan
        .color_context
        .output_color_space
        .color()
        .context("Golden Program Output is not an encoded color space")?;
    let output_boundary = RenderOutputColorBoundary::from_intent(
        RenderOutputColorBoundaryTarget::Export,
        program_output_color,
        &resolved.plan.color_context.output_transform,
        resolved.plan.color_context.output_tone_map,
        resolved.plan.color_context.engine.clone(),
    )?;
    let rgba = execute_cpu_output_boundary(&flattened, &output_boundary)?
        .result
        .frame
        .into_rgba();

    let center = rgba8_at(
        &rgba,
        resolution.width,
        resolution.width / 2,
        resolution.height / 2,
    )?;
    let expected = [
        rec709_code(solid_color.r * solid_color.a * opacity),
        rec709_code(solid_color.g * solid_color.a * opacity),
        rec709_code(solid_color.b * solid_color.a * opacity),
        255,
    ];
    let independent_opacity_oracle_max_error = max_rgb_error(center, expected);
    ensure!(
        independent_opacity_oracle_max_error <= VISUAL_ORACLE_MAX_RGB_ERROR && center[3] == 255,
        "delivery opacity differs from the independent Rec.709 oracle by \
         {independent_opacity_oracle_max_error} codes"
    );
    for (role, x, y) in visual_sample_coordinates(resolution) {
        let sample = rgba8_at(&rgba, resolution.width, x, y)?;
        match role {
            "center" | "left-inside" => ensure!(
                max_rgb_error(sample, expected) <= VISUAL_ORACLE_MAX_RGB_ERROR,
                "{role} sample is outside the independently projected solid"
            ),
            "left-outside" | "top-outside" => ensure!(
                sample[..3].iter().all(|channel| *channel <= 1),
                "{role} sample was not excluded by the authored transform"
            ),
            _ => unreachable!("closed delivery sample role"),
        }
    }

    Ok(ProgramReference {
        resolution,
        rgba_sha256: sha256_bytes(&rgba),
        rgba,
        independent_opacity_oracle_max_error,
    })
}

#[derive(Clone, Copy)]
struct ExactProbeTime {
    numerator: i128,
    denominator: i128,
}

fn exact_probe_time(
    timing: ProbedStreamTiming,
    include_duration: bool,
    stream_kind: &str,
) -> anyhow::Result<ExactProbeTime> {
    let start = timing
        .start_pts
        .with_context(|| format!("{stream_kind} start PTS is unproven"))?;
    let ticks = if include_duration {
        start
            .checked_add(
                timing
                    .duration_ts
                    .with_context(|| format!("{stream_kind} duration TS is unproven"))?,
            )
            .with_context(|| format!("{stream_kind} end timestamp overflowed"))?
    } else {
        start
    };
    let numerator = timing
        .time_base_num
        .filter(|value| *value > 0)
        .with_context(|| format!("{stream_kind} time-base numerator is unproven"))?;
    let denominator = timing
        .time_base_den
        .filter(|value| *value > 0)
        .with_context(|| format!("{stream_kind} time-base denominator is unproven"))?;
    Ok(ExactProbeTime {
        numerator: i128::from(ticks)
            .checked_mul(i128::from(numerator))
            .with_context(|| format!("{stream_kind} timestamp overflowed"))?,
        denominator: i128::from(denominator),
    })
}

fn exact_time_error(left: ExactProbeTime, right: ExactProbeTime) -> anyhow::Result<(i128, i128)> {
    let left_scaled = left
        .numerator
        .checked_mul(right.denominator)
        .context("left probe time comparison overflowed")?;
    let right_scaled = right
        .numerator
        .checked_mul(left.denominator)
        .context("right probe time comparison overflowed")?;
    let denominator = left
        .denominator
        .checked_mul(right.denominator)
        .context("probe time denominator overflowed")?;
    let difference = if left_scaled >= right_scaled {
        left_scaled - right_scaled
    } else {
        right_scaled - left_scaled
    };
    Ok((difference, denominator))
}

fn exact_error_ms(error: (i128, i128)) -> f64 {
    error.0 as f64 * 1_000.0 / error.1 as f64
}

fn ensure_error_within_ms(
    error: (i128, i128),
    allowed_ms: u32,
    description: &str,
) -> anyhow::Result<()> {
    let scaled = error.0.checked_mul(1_000).context("probe timing error overflowed")?;
    let allowed = i128::from(allowed_ms)
        .checked_mul(error.1)
        .context("probe timing allowance overflowed")?;
    ensure!(
        scaled <= allowed,
        "{description} is {:.3} ms, above the {allowed_ms} ms contract",
        exact_error_ms(error)
    );
    Ok(())
}

fn validate_av_boundaries(
    probe: &ExportOutputProbe,
    frame_rate: mondrian_core::Rational,
    duration_frames: i64,
    allowed_error_ms: u32,
) -> anyhow::Result<AvBoundaryEvidence> {
    let video = probe.video.as_ref().context("delivery has no video timing evidence")?;
    let audio = probe.audio.as_ref().context("delivery has no audio timing evidence")?;
    let video_start = exact_probe_time(video.timing, false, "video")?;
    let audio_start = exact_probe_time(audio.timing, false, "audio")?;
    let video_end = exact_probe_time(video.timing, true, "video")?;
    let audio_end = exact_probe_time(audio.timing, true, "audio")?;
    let zero = ExactProbeTime { numerator: 0, denominator: 1 };
    let expected_end = ExactProbeTime {
        numerator: i128::from(duration_frames)
            .checked_mul(i128::from(frame_rate.den))
            .context("expected delivery duration overflowed")?,
        denominator: i128::from(frame_rate.num),
    };
    let video_start_error = exact_time_error(video_start, zero)?;
    let audio_start_error = exact_time_error(audio_start, zero)?;
    let start_alignment_error = exact_time_error(video_start, audio_start)?;
    let video_end_error = exact_time_error(video_end, expected_end)?;
    let audio_end_error = exact_time_error(audio_end, expected_end)?;
    let end_alignment_error = exact_time_error(video_end, audio_end)?;
    for (error, description) in [
        (video_start_error, "video start offset"),
        (audio_start_error, "audio start offset"),
        (start_alignment_error, "A/V start alignment"),
        (video_end_error, "video end boundary"),
        (audio_end_error, "audio end boundary"),
        (end_alignment_error, "A/V end alignment"),
    ] {
        ensure_error_within_ms(error, allowed_error_ms, description)?;
    }
    Ok(AvBoundaryEvidence {
        allowed_error_ms,
        video_start_offset_ms: exact_error_ms(video_start_error),
        audio_start_offset_ms: exact_error_ms(audio_start_error),
        start_alignment_error_ms: exact_error_ms(start_alignment_error),
        video_end_error_ms: exact_error_ms(video_end_error),
        audio_end_error_ms: exact_error_ms(audio_end_error),
        end_alignment_error_ms: exact_error_ms(end_alignment_error),
    })
}

pub(super) fn export_and_probe(
    state: &mut AppState,
    export: &GoldenExportContract,
    output_path: PathBuf,
    range: TimelineExportRange,
    frame_rate: mondrian_core::Rational,
    duration_frames: i64,
    duration_error_max_frames: u32,
    av_boundary_error_max_ms: u32,
    audio_sample_rate: u32,
) -> anyhow::Result<ExportEvidence> {
    let completed = execute_export_job(
        state,
        builtin_preset(&export.builtin_preset_id)?.preset(),
        state.active_sequence().map(|sequence| sequence.id),
        range,
        output_path,
        EXPORT_TIMEOUT,
    )?;
    let diagnostics = completed.diagnostics;
    ensure!(
        diagnostics.color.diagnosed_frames == duration_frames as u64,
        "export diagnosed {} frames instead of the complete {duration_frames}-frame work area",
        diagnostics.color.diagnosed_frames
    );
    let composite = diagnostics.color.composite_diagnostics;
    ensure!(
        composite.float_linear_composites >= duration_frames as u64
            && composite.legacy_rgba8_composites == 0
            && !composite.is_color_domain_blocked(),
        "export did not retain a complete working-linear composite path"
    );
    ensure!(
        diagnostics.color.output_precision_failures == 0
            && diagnostics.color.output_transform_issues == 0,
        "export reported a precision or output-transform correctness failure"
    );
    let output_path = completed.output_path;
    let probe = probe_export_output(&output_path).map_err(anyhow::Error::msg)?;
    assert_probe_matches_contract(
        &probe,
        export,
        frame_rate,
        duration_frames,
        duration_error_max_frames,
        audio_sample_rate,
    )?;
    let av_boundaries = validate_av_boundaries(
        &probe,
        frame_rate,
        duration_frames,
        av_boundary_error_max_ms,
    )?;
    Ok(ExportEvidence {
        export_id: export.id.clone(),
        job_id: completed.job_id,
        generation: completed.generation,
        executed: completed.executed,
        terminal_disposition: completed.terminal_disposition,
        diagnostics,
        output_sha256: sha256_file(&output_path)?,
        output_path,
        probe,
        av_boundaries,
    })
}

fn stereo_signal_evidence(
    start_sample: i64,
    frame_count: usize,
    samples: &[f32],
) -> anyhow::Result<AudioSignalEvidence> {
    ensure!(
        samples.len() == frame_count.saturating_mul(2),
        "audio signal extent does not match the stereo contract"
    );
    ensure!(
        samples.iter().all(|sample| sample.is_finite()),
        "audio signal contains a non-finite sample"
    );
    let mut energy = [0.0_f64; 2];
    let mut peak = [0.0_f32; 2];
    for frame in samples.chunks_exact(2) {
        for channel in 0..2 {
            let sample = frame[channel];
            energy[channel] += f64::from(sample) * f64::from(sample);
            peak[channel] = peak[channel].max(sample.abs());
        }
    }
    let denominator = frame_count.max(1) as f64;
    let evidence = AudioSignalEvidence {
        start_sample,
        frame_count,
        left_rms: (energy[0] / denominator).sqrt(),
        right_rms: (energy[1] / denominator).sqrt(),
        left_peak: peak[0],
        right_peak: peak[1],
    };
    ensure!(
        evidence.left_rms >= AUDIO_MIN_RMS
            && evidence.right_rms >= AUDIO_MIN_RMS
            && evidence.left_peak >= AUDIO_MIN_PEAK
            && evidence.right_peak >= AUDIO_MIN_PEAK,
        "audio signal is silent or too weak to prove the delivery path"
    );
    ensure!(
        (evidence.left_rms - evidence.right_rms).abs() >= AUDIO_MIN_CHANNEL_RMS_DELTA,
        "audio signal does not preserve the intentionally unequal channels"
    );
    Ok(evidence)
}

fn render_program_audio_reference(
    state: &AppState,
    work_area_start: TimelineTime,
    sample_rate: u32,
) -> anyhow::Result<AudioSignalEvidence> {
    const ANALYSIS_OFFSET_NUMERATOR: i64 = 3;
    const ANALYSIS_OFFSET_DENOMINATOR: i64 = 8;
    const ANALYSIS_FRAMES_DIVISOR: usize = 4;

    let rate = AudioSampleRate::new(sample_rate)?;
    let analysis_start = work_area_start.checked_add(TimelineTime::new(
        ANALYSIS_OFFSET_NUMERATOR,
        ANALYSIS_OFFSET_DENOMINATOR,
    )?)?;
    let start_sample = AudioSamplePosition::from_timeline_time(
        analysis_start,
        rate,
        AudioSampleRounding::Nearest,
    )?
    .sample();
    let frame_count = usize::try_from(sample_rate)?
        .checked_div(ANALYSIS_FRAMES_DIVISOR)
        .context("audio analysis extent is zero")?;
    ensure!(frame_count > 0, "audio analysis extent is zero");
    let renderer = TimelineAudioPcmRenderer::new(
        state.active_sequence().context("active Sequence is absent")?.clone(),
        state.sequences().to_vec(),
        state.asset_library_handle().context("Asset Library is absent")?,
        std::sync::Arc::clone(&state.audio_source_cache),
        state.execution_resource_decision().audio.runtime_grant,
        mondrian_audio::AudioAuditionOverlay::default(),
        sample_rate,
        AudioChannelLayout::Stereo,
    )?;
    let buffer = renderer.render(
        AudioPcmRenderRequest {
            start_sample,
            frame_count,
            sample_rate,
            channel_layout: AudioChannelLayout::Stereo,
            continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(1)),
        },
        &ExecutionCancellationToken::new(),
    )?;
    stereo_signal_evidence(start_sample, frame_count, &buffer.samples)
}

fn decode_delivery_audio(
    state: &AppState,
    asset: &mondrian_assets::AssetRecord,
    reference: AudioSignalEvidence,
    work_area_start: TimelineTime,
) -> anyhow::Result<AudioRoundtripEvidence> {
    let media_probe =
        asset.media_probe().context("reimported delivery has no coherent media probe")?;
    let rate = AudioSampleRate::new(
        media_probe
            .primary_audio()
            .context("reimported delivery has no primary audio stream")?
            .sample_rate,
    )?;
    let work_area_start_sample = AudioSamplePosition::from_timeline_time(
        work_area_start,
        rate,
        AudioSampleRounding::Nearest,
    )?
    .sample();
    let source_start_sample = reference
        .start_sample
        .checked_sub(work_area_start_sample)
        .context("delivery audio analysis position overflowed")?;
    ensure!(
        source_start_sample >= 0,
        "delivery audio analysis starts before the exported Work Area"
    );
    let path = asset
        .file_path()
        .context("reimported delivery is not a file-backed audio source")?;
    let current_fingerprint = mondrian_media::MediaFileFingerprint::capture(path);
    let stream = asset
        .audio_components
        .resolve_current(
            mondrian_core::AudioSourceComponentId::primary(),
            media_probe,
            current_fingerprint,
        )
        .context("reimported delivery audio Component binding is not executable")?;
    let selection = mondrian_media::AudioSourceSelection::from_stream(stream, current_fingerprint);
    let reader = state.audio_source_cache.open(path, selection)?;
    ensure!(
        reader.channel_layout() == AudioChannelLayout::Stereo,
        "reimported delivery reader did not retain Stereo layout"
    );
    let mut samples = vec![0.0; reference.frame_count.saturating_mul(2)];
    reader.read_interleaved(source_start_sample, reference.frame_count, &mut samples)?;
    let decoded = stereo_signal_evidence(source_start_sample, reference.frame_count, &samples)?;
    let left_rms_absolute_error = (decoded.left_rms - reference.left_rms).abs();
    let right_rms_absolute_error = (decoded.right_rms - reference.right_rms).abs();
    let max_rms_relative_error = [
        left_rms_absolute_error / reference.left_rms.max(f64::EPSILON),
        right_rms_absolute_error / reference.right_rms.max(f64::EPSILON),
    ]
    .into_iter()
    .fold(0.0_f64, f64::max);
    let max_peak_absolute_error = (decoded.left_peak - reference.left_peak)
        .abs()
        .max((decoded.right_peak - reference.right_peak).abs());
    ensure!(
        left_rms_absolute_error <= AUDIO_ROUNDTRIP_MAX_RMS_ABSOLUTE_ERROR
            && right_rms_absolute_error <= AUDIO_ROUNDTRIP_MAX_RMS_ABSOLUTE_ERROR
            && max_rms_relative_error <= AUDIO_ROUNDTRIP_MAX_RMS_RELATIVE_ERROR
            && max_peak_absolute_error <= AUDIO_ROUNDTRIP_MAX_PEAK_ABSOLUTE_ERROR,
        "decoded AAC differs from the production Program reference: \
         left RMS {left_rms_absolute_error:.6}, right RMS {right_rms_absolute_error:.6}, \
         relative RMS {max_rms_relative_error:.3}, peak {max_peak_absolute_error:.6}"
    );
    Ok(AudioRoundtripEvidence {
        reference,
        decoded,
        left_rms_absolute_error,
        right_rms_absolute_error,
        max_rms_relative_error,
        max_peak_absolute_error,
        allowed_rms_absolute_error: AUDIO_ROUNDTRIP_MAX_RMS_ABSOLUTE_ERROR,
        allowed_rms_relative_error: AUDIO_ROUNDTRIP_MAX_RMS_RELATIVE_ERROR,
        allowed_peak_absolute_error: AUDIO_ROUNDTRIP_MAX_PEAK_ABSOLUTE_ERROR,
    })
}

fn reimport_export(
    state: &mut AppState,
    evidence: &ExportEvidence,
    expected: &GoldenExportContract,
    frame_rate: mondrian_core::Rational,
    audio_sample_rate: u32,
    work_area_start: TimelineTime,
    audio_reference: AudioSignalEvidence,
    reference: &ProgramReference,
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<ReimportEvidence> {
    let before = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(Action::ImportMedia(vec![evidence.output_path.clone()]))?;
    wait_for_media_imports(state)?;
    let imported = state
        .asset_library()
        .context("Asset Library is absent after reimport")?
        .list_assets()?
        .into_iter()
        .filter(|asset| !before.contains(&asset.id))
        .collect::<Vec<_>>();
    ensure!(
        imported.len() == 1,
        "reimport created {} assets instead of one",
        imported.len()
    );
    let asset = &imported[0];
    let media_probe =
        asset.media_probe().context("reimported asset has no coherent media probe")?;
    ensure!(
        asset.kind == AssetKind::Video
            && asset.file_path() == Some(evidence.output_path.as_path())
            && media_probe.container.split(',').any(|identity| identity.trim() == "mp4"),
        "reimport did not retain the finished deliverable identity"
    );
    let video = media_probe.primary_video().context("reimported asset has no video")?;
    let audio = media_probe.primary_audio().context("reimported asset has no audio")?;
    let expected_codec_profile = match (
        expected.expected_delivery.video_codec.as_str(),
        expected.expected_delivery.video_profile.as_str(),
    ) {
        ("h264", "high") => (VideoCodec::H264, VideoCodecProfile::H264High),
        ("hevc", "main10") => (VideoCodec::H265, VideoCodecProfile::HevcMain10),
        (codec, profile) => {
            anyhow::bail!("unsupported Golden reimport codec/profile: {codec}/{profile}")
        }
    };
    let expected_pixel_format = match expected.expected_delivery.pixel_format.as_str() {
        "yuv420p" => PixelFormat::Yuv420p,
        "yuv420p10le" => PixelFormat::Yuv420p10le,
        pixel_format => {
            anyhow::bail!("unsupported Golden reimport pixel format: {pixel_format}")
        }
    };
    ensure!(
        video.codec == expected_codec_profile.0
            && video.codec_profile == expected_codec_profile.1
            && video.width == expected.expected_delivery.width
            && video.height == expected.expected_delivery.height
            && video.frame_rate == frame_rate
            && video.frame_rate_proven
            && video.pixel_format == expected_pixel_format
            && video.pixel_format_proven
            && video.bit_depth == expected.expected_delivery.bit_depth,
        "reimported video representation differs from the validated output"
    );
    ensure!(
        audio.codec == AudioCodec::Aac
            && audio.sample_rate == audio_sample_rate
            && audio.channels == 2
            && audio.channel_layout == ChannelLayout::Stereo,
        "reimported audio representation differs from the validated output"
    );
    ensure!(
        reference.resolution.width == video.width && reference.resolution.height == video.height,
        "Program reference dimensions differ from the reimported delivery"
    );
    let input_color = state
        .active_sequence()
        .context("active Sequence is absent")?
        .settings
        .root_program_color_context(state.project_color_environment())
        .media_input(false);
    let request = PreviewTimelineMediaRequest {
        asset_id: asset.id,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Ignore,
        source_time: TimelineTime::ZERO,
        target_resolution: reference.resolution,
        input_color,
        cpu_working_required: false,
    };
    let decoded = decode_media(state, &request, asset, decode_context)?;
    let decoded_rgba = source_rgba(&decoded.frame)?;
    let mut sampled_pixels = Vec::new();
    let mut max_channel_error = 0;
    for (role, x, y) in visual_sample_coordinates(reference.resolution) {
        let reference_pixel = rgba8_at(&reference.rgba, reference.resolution.width, x, y)?;
        let decoded_pixel = rgba8_at(&decoded_rgba, reference.resolution.width, x, y)?;
        let sample_error = max_rgb_error(reference_pixel, decoded_pixel);
        max_channel_error = max_channel_error.max(sample_error);
        sampled_pixels.push(VisualSampleEvidence {
            role,
            x,
            y,
            reference: reference_pixel,
            decoded: decoded_pixel,
            max_rgb_error: sample_error,
        });
    }
    ensure!(
        max_channel_error <= VISUAL_ROUNDTRIP_MAX_RGB_ERROR,
        "reimported delivery differs from the Program reference by \
         {max_channel_error} encoded codes"
    );
    let transformed_inside_outside_proven = sampled_pixels.iter().all(|sample| match sample.role {
        "center" | "left-inside" => sample.decoded[..3].iter().any(|channel| *channel >= 128),
        "left-outside" | "top-outside" => sample.decoded[..3].iter().all(|channel| *channel <= 16),
        _ => false,
    });
    ensure!(
        transformed_inside_outside_proven,
        "reimported delivery did not retain the independently sampled transform"
    );
    let visual_roundtrip = VisualRoundtripEvidence {
        width: reference.resolution.width,
        height: reference.resolution.height,
        reference_rgba_sha256: reference.rgba_sha256.clone(),
        decoded_rgba_sha256: sha256_bytes(&decoded_rgba),
        sampled_pixels,
        max_channel_error,
        allowed_max_channel_error: VISUAL_ROUNDTRIP_MAX_RGB_ERROR,
        independent_opacity_oracle_max_error: reference.independent_opacity_oracle_max_error,
        allowed_opacity_oracle_max_error: VISUAL_ORACLE_MAX_RGB_ERROR,
        transformed_inside_outside_proven,
    };
    let audio_roundtrip = decode_delivery_audio(state, asset, audio_reference, work_area_start)?;
    Ok(ReimportEvidence {
        export_id: evidence.export_id.clone(),
        asset_id: asset.id,
        container: media_probe.container.clone(),
        video_codec: video.codec.clone(),
        video_profile: video.codec_profile,
        width: video.width,
        height: video.height,
        frame_rate: video.frame_rate,
        pixel_format: video.pixel_format,
        pixel_format_proven: video.pixel_format_proven,
        bit_depth: video.bit_depth,
        audio_codec: audio.codec.clone(),
        audio_sample_rate: audio.sample_rate,
        audio_channels: audio.channels,
        audio_channel_layout: audio.channel_layout.clone(),
        input_color_resolution: decoded.input_color_resolution,
        audio_roundtrip,
        visual_roundtrip,
    })
}

pub(super) fn execute_delivery_stage(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
    output_directory: &Path,
) -> anyhow::Result<GoldenDeliveryReport> {
    let slice = contract
        .execution_slices
        .iter()
        .find(|slice| slice.id == DELIVERY_SLICE_ID)
        .context("delivery Golden execution slice is missing")?;
    ensure!(
        slice.required_fixture_roles == ["pcm-audio"]
            && slice.required_operations == ["trim", "export", "reimport"]
            && slice.required_content == ["transform", "opacity"]
            && slice.required_exports == ["h264-aac-sdr", "hevc-main10"],
        "delivery slice contract drifted"
    );
    let window = slice.timeline_window.context("delivery slice has no timeline window")?;
    let duration_frames = window.end_frame_exclusive - window.start_frame;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let manifest: CorpusManifest = load_json(&root.join("tests/validation/corpus-manifest.json"))?;
    let fixture = resolve_fixture(root, &fixture_root(root), contract, &manifest, "pcm-audio")?;

    let stage = workflow.bind_slice_primary_sequence(contract, DELIVERY_SLICE_ID)?;
    let state = workflow.app_mut();
    let audio_asset = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .find(|asset| asset.file_path() == Some(fixture.path.as_path()))
        .context("Hero delivery requires the Foundation PCM Asset")?;
    ensure!(
        audio_asset.kind == AssetKind::Audio,
        "PCM fixture is not audio"
    );

    let sequence = state.active_sequence().context("active Sequence is absent")?;
    ensure!(
        sequence.id == stage.sequence_id() && sequence.settings == settings,
        "delivery slice is not bound to the contract-owned Hero Sequence"
    );
    let time_base = sequence.time_base();
    let expected_start =
        TimelineTime::from_frame_position(FramePosition::new(window.start_frame, time_base))?;
    let expected_end = TimelineTime::from_frame_position(FramePosition::new(
        window.end_frame_exclusive,
        time_base,
    ))?;
    let mut pcm_placements = Vec::new();
    for track in &sequence.audio_tracks {
        for clip in &track.clips {
            if clip.media_asset_id() == Some(audio_asset.id)
                && !clip.is_disabled
                && clip.position <= expected_start
                && clip.end_position()? >= expected_end
                && clip.audio_components.iter().any(|edit| edit.enabled)
            {
                pcm_placements.push((track.id, clip.id));
            }
        }
    }
    ensure!(
        pcm_placements.len() == 1,
        "Hero delivery requires exactly one enabled Foundation PCM placement covering the work area"
    );
    let (pcm_audio_track_id, pcm_audio_clip_id) = pcm_placements[0];
    let pristine_video_tracks = sequence
        .video_tracks
        .iter()
        .filter(|track| {
            track.clips.is_empty()
                && !track.is_locked
                && !track.is_muted
                && track.is_visible
                && track.blend_mode == BlendMode::Normal
                && track.evaluate_opacity(TimelineTime::ZERO) == 1.0
        })
        .map(|track| track.id)
        .collect::<Vec<_>>();
    ensure!(
        pristine_video_tracks.len() == 1,
        "Hero delivery requires exactly one pristine video Track"
    );
    let video_track_id = pristine_video_tracks[0];

    let asset_library_revision_before_solid =
        state.asset_library().context("Asset Library is absent")?.database_revision()?;
    let solid_asset_id = state.create_solid_color_asset_in_folder(None, None)?;
    let asset_library_revision_after_solid =
        state.asset_library().context("Asset Library is absent")?.database_revision()?;
    ensure!(
        asset_library_revision_after_solid > asset_library_revision_before_solid,
        "solid-color product action did not advance Asset Library authority"
    );
    let solid_asset = state
        .asset_library()
        .context("Asset Library is absent")?
        .get_asset(solid_asset_id)?
        .context("solid-color product Interface created no asset")?;
    ensure!(
        solid_asset.kind == AssetKind::SolidColor,
        "solid-color product Interface created a different Asset kind"
    );

    state.dispatch_action(assets_prepare_drag_action(AssetsPrepareDragPayload {
        asset_id: solid_asset.id,
    }))?;
    let (solid_clip_id, place_solid) = author_transition(state, "place-delivery-solid", |state| {
        state
            .drop_dragging_asset_to_video_track_with_mode(
                video_track_id,
                window.start_frame,
                ClipOverlapMode::Overwrite,
            )
            .map_err(anyhow::Error::from)
    })?;
    let trim_step = dispatch_author_transition(
        state,
        "trim-delivery-solid",
        timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![solid_clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            frame: window.end_frame_exclusive,
        }),
    )?;
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let solid_clip = sequence
        .video_tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == solid_clip_id))
        .context("trim lost solid Clip")?;
    ensure!(
        solid_clip.position == expected_start && solid_clip.end_position()? == expected_end,
        "trim did not produce the exact Golden window"
    );

    let solid_clip_ref = InspectorClipRefPayload {
        track_id: video_track_id,
        is_video_track: true,
        clip_id: solid_clip_id,
    };
    let mut transform_steps = Vec::new();
    for (intent, field, value) in [
        (
            "set-delivery-position-x",
            InspectorClipTransformField::PositionX,
            96.0,
        ),
        (
            "set-delivery-position-y",
            InspectorClipTransformField::PositionY,
            54.0,
        ),
        (
            "set-delivery-scale",
            InspectorClipTransformField::ScalePercent,
            90.0,
        ),
    ] {
        transform_steps.push(dispatch_author_transition(
            state,
            intent,
            inspector_set_clip_transform_field_action(InspectorSetClipTransformFieldPayload {
                clip: solid_clip_ref,
                field,
                value,
            }),
        )?);
    }
    let opacity_step = dispatch_author_transition(
        state,
        "set-delivery-opacity",
        inspector_set_clip_opacity_action(InspectorSetClipOpacityPayload {
            clip: solid_clip_ref,
            opacity_percent: 80.0,
        }),
    )?;
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let solid_clip = sequence
        .video_tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == solid_clip_id))
        .context("authored solid Clip is absent")?;
    let position = solid_clip.transform.get_position(solid_clip.clip_time_in);
    let scale = solid_clip.transform.get_scale(solid_clip.clip_time_in);
    let opacity = solid_clip.transform.evaluate_opacity(solid_clip.clip_time_in);
    ensure!(
        position.x == 96.0
            && position.y == 54.0
            && scale.x == 0.9
            && scale.y == 0.9
            && (opacity - 0.8).abs() < 1.0e-6,
        "authored transform/opacity did not survive the product actions"
    );
    let content = vec![
        ContentEvidence::Transform {
            author_steps: transform_steps,
            position_x: position.x,
            position_y: position.y,
            scale_x: scale.x,
            scale_y: scale.y,
        },
        ContentEvidence::Opacity { author_step: opacity_step, value: opacity },
    ];
    let authoring = capture_delivery_authoring_anchor(
        state,
        stage.sequence_id(),
        video_track_id,
        solid_clip_id,
        solid_asset.id,
    )?;
    let sequence_before_execution =
        serde_json::to_value(state.active_sequence().context("active Sequence is absent")?)?;
    let durable_reopen = workflow.durable_save_reopen_for(&stage)?;
    let state = workflow.app_mut();
    ensure!(
        serde_json::to_value(state.active_sequence().context("active Sequence is absent")?)?
            == sequence_before_execution,
        "durable reopen changed Hero delivery authoring"
    );
    ensure!(
        capture_delivery_authoring_anchor(
            state,
            stage.sequence_id(),
            video_track_id,
            solid_clip_id,
            solid_asset.id,
        )? == authoring,
        "durable reopen changed delivery authoring"
    );
    let audio_reference =
        render_program_audio_reference(state, expected_start, settings.audio_sample_rate)?;

    let range = TimelineExportRange::WorkArea {
        start_frame: window.start_frame,
        end_frame_exclusive: window.end_frame_exclusive,
    };
    let mut exports = Vec::new();
    let mut program_references = Vec::new();
    for export_id in &slice.required_exports {
        let export = contract
            .exports
            .iter()
            .find(|export| &export.id == export_id)
            .with_context(|| format!("Golden export contract is absent: {export_id}"))?;
        let resolution = Resolution {
            width: export.expected_delivery.width,
            height: export.expected_delivery.height,
        };
        program_references.push(render_program_reference(
            state,
            window.start_frame,
            resolution,
            solid_clip_id,
        )?);
        exports.push(export_and_probe(
            state,
            export,
            output_directory.join(format!("{export_id}.mp4")),
            range,
            settings.frame_rate,
            duration_frames,
            contract.acceptance.duration_error_max_frames,
            contract.acceptance.av_boundary_error_max_ms,
            settings.audio_sample_rate,
        )?);
    }
    let mut reimports = Vec::new();
    let mut decode_context = PreviewDecodeSessionContext::new();
    for (evidence, reference) in exports.iter().zip(&program_references) {
        let expected = contract
            .exports
            .iter()
            .find(|export| export.id == evidence.export_id)
            .context("export evidence lost its Golden contract")?;
        reimports.push(reimport_export(
            state,
            evidence,
            expected,
            settings.frame_rate,
            settings.audio_sample_rate,
            expected_start,
            audio_reference,
            reference,
            &mut decode_context,
        )?);
    }
    ensure!(
        reimports.len() == 2,
        "delivery audio comparison requires both contracted exports"
    );
    let left_rms_delta = (reimports[0].audio_roundtrip.decoded.left_rms
        - reimports[1].audio_roundtrip.decoded.left_rms)
        .abs();
    let right_rms_delta = (reimports[0].audio_roundtrip.decoded.right_rms
        - reimports[1].audio_roundtrip.decoded.right_rms)
        .abs();
    ensure!(
        left_rms_delta <= AUDIO_CODEC_AGREEMENT_MAX_RMS_DELTA
            && right_rms_delta <= AUDIO_CODEC_AGREEMENT_MAX_RMS_DELTA,
        "H.264/AAC and HEVC/AAC deliveries disagree: \
         left RMS delta {left_rms_delta:.6}, right RMS delta {right_rms_delta:.6}"
    );
    let audio_codec_agreement = AudioCodecAgreementEvidence {
        left_rms_delta,
        right_rms_delta,
        allowed_rms_delta: AUDIO_CODEC_AGREEMENT_MAX_RMS_DELTA,
    };
    ensure!(
        serde_json::to_value(state.active_sequence().context("active Sequence is absent")?)?
            == sequence_before_execution,
        "export/reimport changed the Hero Sequence author state"
    );
    ensure!(
        capture_delivery_authoring_anchor(
            state,
            stage.sequence_id(),
            video_track_id,
            solid_clip_id,
            solid_asset.id,
        )? == authoring,
        "export/reimport changed delivery authoring"
    );
    let operations = vec![
        OperationEvidence::Trim {
            author_step: trim_step,
            clip_ids: vec![solid_clip_id],
            end_frame_exclusive: window.end_frame_exclusive,
        },
        OperationEvidence::Export { deliveries: exports },
        OperationEvidence::Reimport { assets: reimports, audio_codec_agreement },
    ];
    ensure_exact_requirement_evidence(
        &slice.required_operations,
        operations.iter().map(OperationEvidence::id),
        "operation",
    )?;
    ensure_exact_requirement_evidence(
        &slice.required_content,
        content.iter().map(ContentEvidence::id),
        "content",
    )?;
    workflow.verify_binding()?;

    Ok(GoldenDeliveryReport {
        schema_version: 6,
        profile: DELIVERY_SLICE_ID,
        contract_id: contract.id.clone(),
        corpus_revision: manifest.corpus_revision,
        status: "passed",
        complete_golden_project: false,
        fixture,
        setup: DeliverySetupEvidence {
            project_path: workflow.project_path().to_path_buf(),
            stage,
            video_track_id,
            pcm_audio_track_id,
            solid_asset_id: solid_asset.id,
            pcm_audio_asset_id: audio_asset.id,
            solid_clip_id,
            pcm_audio_clip_id,
            asset_library_revision_before_solid,
            asset_library_revision_after_solid,
            place_solid,
            durable_reopen,
            start_frame: window.start_frame,
            end_frame_exclusive: window.end_frame_exclusive,
        },
        operations,
        content,
        authoring,
    })
}
