//! 媒体文件元数据探针
//!
//! 使用 FFmpeg `avformat_open_input` 读取媒体文件的流信息。对已经由
//! CICP/容器信息识别为 HDR 的视频，额外解码首帧以捕获只存在于
//! `AVFrameSideData` 的静态/动态 HDR 元数据。图片文件在容器未报告
//! 帧数时最多解码到第二帧或 EOF，以区分已证明单帧与动画/未知输入。

use crate::decoder::decoded_video_range_from_ffmpeg;
use ffmpeg_next as ffmpeg;
use mondrian_core::icc::parse_icc_display_profile;
use mondrian_core::types::*;
pub use mondrian_core::{
    is_picture_file_extension, resolve_video_color_metadata_declarations, AudioCodec,
    AudioStreamInfo, ChannelLayout, DecodedVideoRange, DetectedColorInterpretation, MediaInfo,
    MediaProbeSnapshot, PixelFormat, ProResVariant, ProvenVideoSampling, VideoCodec,
    VideoCodecProfile, VideoColorDetectionMethod, VideoColorInterpretationConfidence,
    VideoColorInterpretationEvidence, VideoColorInterpretationWarning, VideoColorMetadata,
    VideoColorMetadataDeclaration, VideoColorMetadataDeclarationResolution, VideoColorMetadataHint,
    VideoColorMetadataHintAuthority, VideoColorMetadataHintScope, VideoColorSpaceSource,
    VideoColorTag, VideoHdrMetadataSummary, VideoHdrSideDataKind, VideoStreamInfo,
};
use mondrian_core::{
    AudioChannelLayout, AudioChannelPosition, VideoContentLightMetadata, VideoHdrChromaticity,
    VideoHdrMetadataPayload, VideoHdrRational, VideoIccProfileMetadata,
    VideoMasteringDisplayLuminance, VideoMasteringDisplayMetadata, VideoMasteringDisplayPrimaries,
};
use serde::{Deserialize, Serialize};
use std::os::raw::c_int;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
struct IccColorProfileHint {
    mapping: mondrian_core::icc::IccColorSpaceMapping,
    profile_name: Option<String>,
}

/// Diagnostic snapshot of a video stream's color metadata interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorDiagnostic {
    /// Encoded quantization range reported by FFmpeg.
    #[serde(default)]
    pub color_range: DecodedVideoRange,
    /// Proven physical sampling family used to validate matrix obligations.
    #[serde(default)]
    pub sampling: Option<ProvenVideoSampling>,
    /// Structured interpretation of automatic color metadata.
    pub interpretation: DetectedColorInterpretation,
    /// Raw CICP-style metadata captured from FFmpeg, when available.
    pub metadata: Option<VideoColorMetadata>,
    /// Metadata hints that contributed to identifying acquisition/log color space.
    #[serde(default)]
    pub metadata_hints: Vec<VideoColorMetadataHint>,
    /// HDR-related stream side-data summaries.
    #[serde(default)]
    pub hdr_metadata: Vec<VideoHdrMetadataSummary>,
}

/// Machine-readable issue summary for a probed video stream's color metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorDiagnosticIssueSummary {
    /// Executable metadata identity, excluding diagnostic-only suggestions.
    pub executable_color_space: Option<ColorSpace>,
    /// Confidence of the automatic interpretation.
    pub confidence: VideoColorInterpretationConfidence,
    /// Source state for the color metadata decision.
    pub source: VideoColorSpaceSource,
    /// Method that produced the color metadata decision.
    pub method: VideoColorDetectionMethod,
    /// Whether raw CICP/FFmpeg metadata was captured.
    pub has_raw_cicp_metadata: bool,
    /// Number of metadata hints captured from container and stream metadata.
    pub metadata_hint_count: u64,
    /// Number of interpretation evidence records.
    pub evidence_count: u64,
    /// Number of interpretation warnings.
    pub warning_count: u64,
    /// Number of multiple-hint ambiguity warnings.
    pub multiple_metadata_hints: u64,
    /// Number of ignored metadata hints across multiple-hint warnings.
    pub ignored_metadata_hints: u64,
    /// Number of warnings where a metadata hint overrode conflicting CICP tags.
    pub metadata_hint_overrides_cicp_tags: u64,
    /// Number of warnings where stronger evidence overrode free-form metadata hints.
    pub lower_priority_metadata_hints: u64,
    /// Number of lower-priority metadata hints retained but not selected.
    pub ignored_lower_priority_metadata_hints: u64,
    /// Number of partial CICP inference warnings.
    pub partial_cicp_tags: u64,
    /// Number of missing CICP warnings.
    pub missing_cicp_tags: u64,
    /// Number of unsupported CICP warnings.
    pub unsupported_cicp_tags: u64,
    /// Number of decoder-unavailable warnings.
    pub decoder_unavailable: u64,
    /// Number of HDR side-data records captured.
    pub hdr_side_data_count: u64,
    /// Whether mastering-display HDR metadata was present.
    pub has_mastering_display_metadata: bool,
    /// Whether content-light HDR metadata was present.
    pub has_content_light_metadata: bool,
    /// Whether HDR10+ side data was present.
    pub has_dynamic_hdr10_plus: bool,
    /// Whether Dolby Vision configuration side data was present.
    pub has_dolby_vision_config: bool,
    /// Whether ICC profile side data was present.
    pub has_icc_profile: bool,
    /// Number of ICC-vs-CICP mismatch warnings.
    #[serde(default)]
    pub icc_cicp_mismatch: u64,
    /// Number of parsed but unmapped ICC profile warnings.
    #[serde(default)]
    pub icc_profile_unmapped: u64,
    /// Whether the stream has warnings that should be shown to users.
    pub has_user_visible_warnings: bool,
}

/// Aggregated issue summary across multiple probed video color diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VideoColorDiagnosticIssueAggregate {
    /// Number of diagnostics aggregated.
    pub diagnostics: u64,
    /// Diagnostics that resolved to an explicit Mondrian color space.
    pub diagnostics_with_executable_color_space: u64,
    /// Diagnostics that carried one or more user-visible warnings.
    pub diagnostics_with_warnings: u64,
    /// Diagnostics that preserved raw CICP/FFmpeg metadata.
    pub diagnostics_with_raw_cicp_metadata: u64,
    /// Diagnostics that carried one or more metadata hints.
    pub diagnostics_with_metadata_hints: u64,
    /// Diagnostics that carried one or more HDR side-data records.
    pub diagnostics_with_hdr_metadata: u64,
    /// Diagnostics whose final decision came from metadata hints.
    pub method_metadata_hint: u64,
    /// Diagnostics whose final decision came from ICC profile metadata.
    #[serde(default)]
    pub method_icc_profile: u64,
    /// Diagnostics whose final decision came from CICP tags.
    pub method_cicp_tags: u64,
    /// Diagnostics that fell through to missing-metadata handling.
    pub method_missing_metadata: u64,
    /// Diagnostics whose CICP tags were present but unsupported.
    pub method_unsupported_cicp_tags: u64,
    /// Diagnostics that could not probe due to decoder unavailability.
    pub method_decoder_unavailable: u64,
    /// Diagnostics reported with high confidence.
    pub confidence_high: u64,
    /// Diagnostics reported with medium confidence.
    pub confidence_medium: u64,
    /// Diagnostics reported with low confidence.
    pub confidence_low: u64,
    /// Diagnostics reported with no confidence.
    pub confidence_none: u64,
    /// Total metadata hints observed across all diagnostics.
    pub metadata_hint_count: u64,
    /// Total evidence records observed across all diagnostics.
    pub evidence_count: u64,
    /// Total warnings observed across all diagnostics.
    pub warning_count: u64,
    /// Total multiple-hint ambiguity warnings.
    pub multiple_metadata_hints: u64,
    /// Total ignored metadata hints across multiple-hint warnings.
    pub ignored_metadata_hints: u64,
    /// Total hint-vs-CICP conflict warnings.
    pub metadata_hint_overrides_cicp_tags: u64,
    /// Total warnings where stronger evidence overrode free-form metadata hints.
    pub lower_priority_metadata_hints: u64,
    /// Total lower-priority metadata hints retained but not selected.
    pub ignored_lower_priority_metadata_hints: u64,
    /// Total partial CICP inference warnings.
    pub partial_cicp_tags: u64,
    /// Total missing CICP warnings.
    pub missing_cicp_tags: u64,
    /// Total unsupported CICP warnings.
    pub unsupported_cicp_tags: u64,
    /// Total decoder-unavailable warnings.
    pub decoder_unavailable: u64,
    /// Total HDR side-data records observed across all diagnostics.
    pub hdr_side_data_count: u64,
    /// Diagnostics that carried mastering-display HDR metadata.
    pub diagnostics_with_mastering_display_metadata: u64,
    /// Diagnostics that carried content-light HDR metadata.
    pub diagnostics_with_content_light_metadata: u64,
    /// Diagnostics that carried HDR10+ metadata.
    pub diagnostics_with_dynamic_hdr10_plus: u64,
    /// Diagnostics that carried Dolby Vision configuration metadata.
    pub diagnostics_with_dolby_vision_config: u64,
    /// Diagnostics that carried ICC profile metadata.
    pub diagnostics_with_icc_profile: u64,
    /// Diagnostics with ICC-vs-CICP mismatch warnings.
    #[serde(default)]
    pub diagnostics_with_icc_cicp_mismatch: u64,
    /// Diagnostics containing parsed but unmapped ICC profiles.
    #[serde(default)]
    pub diagnostics_with_icc_profile_unmapped: u64,
}

impl VideoColorDiagnostic {
    /// Build a color diagnostic snapshot from probed stream metadata.
    pub fn from_stream(stream: &VideoStreamInfo) -> Self {
        Self {
            color_range: stream.color_range,
            sampling: stream.proven_sampling(),
            interpretation: stream.color_interpretation.clone(),
            metadata: stream.color_metadata.clone(),
            metadata_hints: stream.color_metadata_hints.clone(),
            hdr_metadata: stream.hdr_metadata.clone(),
        }
    }

    /// Return the sole metadata-derived identity allowed to drive pixels.
    pub fn executable_color_space(&self) -> Option<ColorSpace> {
        self.interpretation.executable_color_space_from_probe(
            self.sampling,
            self.metadata.as_ref(),
            &self.metadata_hints,
        )
    }

    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        let candidate = self
            .interpretation
            .candidate_color_space
            .map(|color_space| format!("{color_space:?}"))
            .unwrap_or_else(|| "None".to_string());
        let executable = self
            .executable_color_space()
            .map(|color_space| format!("{color_space:?}"))
            .unwrap_or_else(|| "None".to_string());
        let metadata = self
            .metadata
            .as_ref()
            .map(VideoColorMetadata::summary)
            .unwrap_or_else(|| "unavailable".to_string());
        let hints = if self.metadata_hints.is_empty() {
            "none".to_string()
        } else {
            self.metadata_hints
                .iter()
                .map(VideoColorMetadataHint::summary)
                .collect::<Vec<_>>()
                .join("|")
        };
        let hdr = if self.hdr_metadata.is_empty() {
            "none".to_string()
        } else {
            self.hdr_metadata
                .iter()
                .map(VideoHdrMetadataSummary::summary)
                .collect::<Vec<_>>()
                .join("|")
        };
        let warnings = if self.interpretation.warnings.is_empty() {
            "none".to_string()
        } else {
            self.interpretation
                .warnings
                .iter()
                .map(VideoColorInterpretationWarning::summary)
                .collect::<Vec<_>>()
                .join("|")
        };
        format!(
            "source={:?},method={:?},candidate={},executable={},range={:?},confidence={:?},overridable={},warnings={},metadata={},hints={},hdr={}",
            self.interpretation.source,
            self.interpretation.method,
            candidate,
            executable,
            self.color_range,
            self.interpretation.confidence,
            self.interpretation.user_overridable,
            warnings,
            metadata,
            hints,
            hdr
        )
    }

    /// Machine-readable issue summary for UI, export reports, and telemetry.
    pub fn issue_summary(&self) -> VideoColorDiagnosticIssueSummary {
        let mut summary = VideoColorDiagnosticIssueSummary {
            executable_color_space: self.executable_color_space(),
            confidence: self.interpretation.confidence,
            source: self.interpretation.source,
            method: self.interpretation.method,
            has_raw_cicp_metadata: self.metadata.is_some(),
            metadata_hint_count: self.metadata_hints.len() as u64,
            evidence_count: self.interpretation.evidence.len() as u64,
            warning_count: self.interpretation.warnings.len() as u64,
            multiple_metadata_hints: 0,
            ignored_metadata_hints: 0,
            metadata_hint_overrides_cicp_tags: 0,
            lower_priority_metadata_hints: 0,
            ignored_lower_priority_metadata_hints: 0,
            partial_cicp_tags: 0,
            missing_cicp_tags: 0,
            unsupported_cicp_tags: 0,
            decoder_unavailable: 0,
            hdr_side_data_count: self.hdr_metadata.len() as u64,
            has_mastering_display_metadata: false,
            has_content_light_metadata: false,
            has_dynamic_hdr10_plus: false,
            has_dolby_vision_config: false,
            has_icc_profile: false,
            icc_cicp_mismatch: 0,
            icc_profile_unmapped: 0,
            has_user_visible_warnings: !self.interpretation.warnings.is_empty(),
        };

        for warning in &self.interpretation.warnings {
            match warning {
                VideoColorInterpretationWarning::MultipleMetadataHints { ignored, .. } => {
                    summary.multiple_metadata_hints =
                        summary.multiple_metadata_hints.saturating_add(1);
                    summary.ignored_metadata_hints =
                        summary.ignored_metadata_hints.saturating_add(ignored.len() as u64);
                }
                VideoColorInterpretationWarning::MetadataHintOverridesCicpTags { .. } => {
                    summary.metadata_hint_overrides_cicp_tags =
                        summary.metadata_hint_overrides_cicp_tags.saturating_add(1);
                }
                VideoColorInterpretationWarning::DescriptiveMetadataHintInference { .. } => {}
                VideoColorInterpretationWarning::LowerPriorityMetadataHints { ignored, .. } => {
                    summary.lower_priority_metadata_hints =
                        summary.lower_priority_metadata_hints.saturating_add(1);
                    summary.ignored_lower_priority_metadata_hints = summary
                        .ignored_lower_priority_metadata_hints
                        .saturating_add(ignored.len() as u64);
                }
                VideoColorInterpretationWarning::PartialCicpTags { .. } => {
                    summary.partial_cicp_tags = summary.partial_cicp_tags.saturating_add(1);
                }
                VideoColorInterpretationWarning::MissingCicpTags => {
                    summary.missing_cicp_tags = summary.missing_cicp_tags.saturating_add(1);
                }
                VideoColorInterpretationWarning::UnsupportedCicpTags => {
                    summary.unsupported_cicp_tags = summary.unsupported_cicp_tags.saturating_add(1);
                }
                VideoColorInterpretationWarning::DecoderUnavailable => {
                    summary.decoder_unavailable = summary.decoder_unavailable.saturating_add(1);
                }
                VideoColorInterpretationWarning::IccCicpMismatch { .. } => {
                    summary.icc_cicp_mismatch = summary.icc_cicp_mismatch.saturating_add(1);
                }
                VideoColorInterpretationWarning::IccProfileUnmapped { .. } => {
                    summary.icc_profile_unmapped = summary.icc_profile_unmapped.saturating_add(1);
                }
            }
        }

        for hdr in &self.hdr_metadata {
            match hdr.kind {
                VideoHdrSideDataKind::MasteringDisplayMetadata => {
                    summary.has_mastering_display_metadata = true;
                }
                VideoHdrSideDataKind::ContentLightLevel => {
                    summary.has_content_light_metadata = true;
                }
                VideoHdrSideDataKind::DynamicHdr10Plus => {
                    summary.has_dynamic_hdr10_plus = true;
                }
                VideoHdrSideDataKind::DolbyVisionConfig => {
                    summary.has_dolby_vision_config = true;
                }
                VideoHdrSideDataKind::IccProfile => {
                    summary.has_icc_profile = true;
                }
            }
        }

        summary
    }
}

impl VideoColorDiagnosticIssueAggregate {
    /// Observe one diagnostic summary.
    pub fn observe_summary(&mut self, summary: VideoColorDiagnosticIssueSummary) {
        self.diagnostics = self.diagnostics.saturating_add(1);
        self.diagnostics_with_executable_color_space = self
            .diagnostics_with_executable_color_space
            .saturating_add(u64::from(summary.executable_color_space.is_some()));
        self.diagnostics_with_warnings = self
            .diagnostics_with_warnings
            .saturating_add(u64::from(summary.has_user_visible_warnings));
        self.diagnostics_with_raw_cicp_metadata = self
            .diagnostics_with_raw_cicp_metadata
            .saturating_add(u64::from(summary.has_raw_cicp_metadata));
        self.diagnostics_with_metadata_hints = self
            .diagnostics_with_metadata_hints
            .saturating_add(u64::from(summary.metadata_hint_count > 0));
        self.diagnostics_with_hdr_metadata = self
            .diagnostics_with_hdr_metadata
            .saturating_add(u64::from(summary.hdr_side_data_count > 0));
        self.metadata_hint_count =
            self.metadata_hint_count.saturating_add(summary.metadata_hint_count);
        self.evidence_count = self.evidence_count.saturating_add(summary.evidence_count);
        self.warning_count = self.warning_count.saturating_add(summary.warning_count);
        self.multiple_metadata_hints =
            self.multiple_metadata_hints.saturating_add(summary.multiple_metadata_hints);
        self.ignored_metadata_hints =
            self.ignored_metadata_hints.saturating_add(summary.ignored_metadata_hints);
        self.metadata_hint_overrides_cicp_tags = self
            .metadata_hint_overrides_cicp_tags
            .saturating_add(summary.metadata_hint_overrides_cicp_tags);
        self.lower_priority_metadata_hints = self
            .lower_priority_metadata_hints
            .saturating_add(summary.lower_priority_metadata_hints);
        self.ignored_lower_priority_metadata_hints = self
            .ignored_lower_priority_metadata_hints
            .saturating_add(summary.ignored_lower_priority_metadata_hints);
        self.partial_cicp_tags = self.partial_cicp_tags.saturating_add(summary.partial_cicp_tags);
        self.missing_cicp_tags = self.missing_cicp_tags.saturating_add(summary.missing_cicp_tags);
        self.unsupported_cicp_tags =
            self.unsupported_cicp_tags.saturating_add(summary.unsupported_cicp_tags);
        self.decoder_unavailable =
            self.decoder_unavailable.saturating_add(summary.decoder_unavailable);
        self.hdr_side_data_count =
            self.hdr_side_data_count.saturating_add(summary.hdr_side_data_count);
        self.diagnostics_with_mastering_display_metadata = self
            .diagnostics_with_mastering_display_metadata
            .saturating_add(u64::from(summary.has_mastering_display_metadata));
        self.diagnostics_with_content_light_metadata = self
            .diagnostics_with_content_light_metadata
            .saturating_add(u64::from(summary.has_content_light_metadata));
        self.diagnostics_with_dynamic_hdr10_plus = self
            .diagnostics_with_dynamic_hdr10_plus
            .saturating_add(u64::from(summary.has_dynamic_hdr10_plus));
        self.diagnostics_with_dolby_vision_config = self
            .diagnostics_with_dolby_vision_config
            .saturating_add(u64::from(summary.has_dolby_vision_config));
        self.diagnostics_with_icc_profile = self
            .diagnostics_with_icc_profile
            .saturating_add(u64::from(summary.has_icc_profile));
        self.diagnostics_with_icc_cicp_mismatch = self
            .diagnostics_with_icc_cicp_mismatch
            .saturating_add(summary.icc_cicp_mismatch);
        self.diagnostics_with_icc_profile_unmapped = self
            .diagnostics_with_icc_profile_unmapped
            .saturating_add(summary.icc_profile_unmapped);

        match summary.method {
            VideoColorDetectionMethod::MetadataHint => {
                self.method_metadata_hint = self.method_metadata_hint.saturating_add(1);
            }
            VideoColorDetectionMethod::IccProfile => {
                self.method_icc_profile = self.method_icc_profile.saturating_add(1);
            }
            VideoColorDetectionMethod::CicpTags => {
                self.method_cicp_tags = self.method_cicp_tags.saturating_add(1);
            }
            VideoColorDetectionMethod::MissingMetadata => {
                self.method_missing_metadata = self.method_missing_metadata.saturating_add(1);
            }
            VideoColorDetectionMethod::UnsupportedCicpTags => {
                self.method_unsupported_cicp_tags =
                    self.method_unsupported_cicp_tags.saturating_add(1);
            }
            VideoColorDetectionMethod::DecoderUnavailable => {
                self.method_decoder_unavailable = self.method_decoder_unavailable.saturating_add(1);
            }
        }

        match summary.confidence {
            VideoColorInterpretationConfidence::High => {
                self.confidence_high = self.confidence_high.saturating_add(1);
            }
            VideoColorInterpretationConfidence::Medium => {
                self.confidence_medium = self.confidence_medium.saturating_add(1);
            }
            VideoColorInterpretationConfidence::Low => {
                self.confidence_low = self.confidence_low.saturating_add(1);
            }
            VideoColorInterpretationConfidence::None => {
                self.confidence_none = self.confidence_none.saturating_add(1);
            }
        }
    }

    /// Observe one full video color diagnostic.
    pub fn observe(&mut self, diagnostic: &VideoColorDiagnostic) {
        self.observe_summary(diagnostic.issue_summary());
    }

    /// Aggregate multiple full video color diagnostics.
    pub fn from_diagnostics<'a, I>(diagnostics: I) -> Self
    where
        I: IntoIterator<Item = &'a VideoColorDiagnostic>,
    {
        let mut aggregate = Self::default();
        for diagnostic in diagnostics {
            aggregate.observe(diagnostic);
        }
        aggregate
    }
}

/// Probe one file into the stable, immutable media contract.
///
/// This synchronous FFmpeg Adapter must run on bounded media execution owned
/// by its caller. It performs no Asset Library mutation.
pub fn probe_media_info(path: &Path) -> mondrian_core::Result<MediaProbeSnapshot> {
    let started_at = Instant::now();
    tracing::info!("Probing media file: {:?}", path);

    if !path.exists() {
        return Err(mondrian_core::MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: "文件不存在".to_string(),
        });
    }

    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    crate::ffmpeg_runtime::ensure_ffmpeg_initialized(path)?;

    let input =
        ffmpeg::format::input(path).map_err(|e| mondrian_core::MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

    let duration = if input.duration() > 0 {
        Duration::from_micros(input.duration() as u64)
    } else {
        Duration::ZERO
    };

    let container = input.format().name().to_lowercase();
    let container_color_hints =
        collect_color_metadata_hints(VideoColorMetadataHintScope::Container, &input.metadata());
    let file_name_color_hint =
        path.file_name().and_then(|file_name| file_name.to_str()).and_then(|file_name| {
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::FileName,
                "filename",
                file_name,
            )
        });

    let mut video_streams = Vec::new();
    let mut audio_streams = Vec::new();

    for stream in input.streams() {
        let params = stream.parameters();
        match params.medium() {
            ffmpeg::media::Type::Video => {
                let stream_duration =
                    duration_from_stream_ticks(stream.duration(), stream.time_base());
                let mut color_metadata_hints = collect_color_metadata_hints(
                    VideoColorMetadataHintScope::Stream,
                    &stream.metadata(),
                );
                color_metadata_hints.extend(container_color_hints.clone());
                color_metadata_hints.extend(file_name_color_hint.clone());
                color_metadata_hints.sort_by(metadata_hint_diagnostic_order);
                let hdr_metadata = collect_hdr_metadata_summaries(&stream);
                let mut width = 0;
                let mut height = 0;
                let mut pixel_format = PixelFormat::Yuv420p;
                let mut pixel_format_proven = false;
                let mut bit_depth = 8;
                let mut has_alpha = false;

                if let Ok(context) =
                    ffmpeg::codec::context::Context::from_parameters(params.clone())
                    && let Ok(decoder) = context.decoder().video()
                {
                    width = decoder.width();
                    height = decoder.height();
                    if let Some(probed_pixel_format) = map_pixel_format(decoder.format()) {
                        pixel_format = probed_pixel_format;
                        pixel_format_proven = true;
                    }
                    let color_range = decoded_video_range_from_ffmpeg(decoder.color_range());
                    bit_depth = pixel_format.bit_depth();
                    has_alpha = pixel_format.has_alpha();
                    let raw_color_metadata = capture_color_metadata(
                        decoder.color_primaries(),
                        decoder.color_transfer_characteristic(),
                        decoder.color_space(),
                    );
                    let color_interpretation = detect_color_space_from_metadata(
                        &raw_color_metadata,
                        &color_metadata_hints,
                        icc_color_profile_hint(&hdr_metadata).as_ref(),
                        pixel_format_proven.then_some(ProvenVideoSampling {
                            pixel_format,
                            bit_depth,
                            has_alpha,
                        }),
                    );
                    let (frame_rate, frame_rate_proven) = map_rational(stream.avg_frame_rate());
                    video_streams.push(VideoStreamInfo {
                        index: stream.index() as u32,
                        codec: map_video_codec(params.id()),
                        duration: stream_duration,
                        codec_profile: map_video_codec_profile(decoder.profile()),
                        width,
                        height,
                        frame_rate,
                        frame_rate_proven,
                        pixel_format,
                        pixel_format_proven,
                        color_range,
                        color_interpretation,
                        color_metadata: Some(raw_color_metadata),
                        color_metadata_hints,
                        hdr_metadata,
                        bit_depth,
                        has_alpha,
                        avg_bitrate: 0,
                        total_frames: if stream.frames() > 0 {
                            Some(stream.frames() as u64)
                        } else {
                            None
                        },
                    });
                    continue;
                }

                let (frame_rate, frame_rate_proven) = map_rational(stream.avg_frame_rate());
                let total_frames = if stream.frames() > 0 {
                    Some(stream.frames() as u64)
                } else {
                    None
                };

                video_streams.push(VideoStreamInfo {
                    index: stream.index() as u32,
                    codec: map_video_codec(params.id()),
                    duration: stream_duration,
                    codec_profile: VideoCodecProfile::Unknown,
                    width,
                    height,
                    frame_rate,
                    frame_rate_proven,
                    pixel_format,
                    pixel_format_proven,
                    color_range: DecodedVideoRange::Unknown,
                    color_interpretation: DetectedColorInterpretation::decoder_unavailable(),
                    color_metadata: None,
                    color_metadata_hints,
                    hdr_metadata,
                    bit_depth,
                    has_alpha,
                    avg_bitrate: 0,
                    total_frames,
                });
            }
            ffmpeg::media::Type::Audio => {
                let stream_duration =
                    duration_from_stream_ticks(stream.duration(), stream.time_base());
                let mut sample_rate = 0u32;
                let mut channels = 0u8;
                let mut channel_layout = ChannelLayout::Unspecified(0);
                let mut bit_depth = 16u16;

                if let Ok(context) =
                    ffmpeg::codec::context::Context::from_parameters(params.clone())
                    && let Ok(decoder) = context.decoder().audio()
                {
                    sample_rate = decoder.rate();
                    channels = decoder.channels() as u8;
                    channel_layout = map_channel_layout(decoder.channel_layout(), channels);
                    bit_depth = 16;
                }

                let metadata = stream.metadata();
                let language = normalized_stream_metadata(metadata.get("language"));
                let title = normalized_stream_metadata(metadata.get("title"));
                let stream_id = (stream.id() >= 0).then_some(stream.id());
                let is_default =
                    stream.disposition().contains(ffmpeg::format::stream::Disposition::DEFAULT);

                audio_streams.push(AudioStreamInfo {
                    index: stream.index() as u32,
                    stream_id,
                    language,
                    title,
                    is_default,
                    codec: map_audio_codec(params.id()),
                    duration: stream_duration,
                    sample_rate,
                    channels,
                    channel_layout,
                    bit_depth,
                    avg_bitrate: 0,
                });
            }
            _ => {}
        }
    }

    if is_picture_file_extension(path) {
        for video in &mut video_streams {
            if video.total_frames.is_some() {
                continue;
            }
            match probe_exactly_one_video_frame(path, video.index) {
                Ok(true) => video.total_frames = Some(1),
                Ok(false) => {}
                Err(reason) => {
                    tracing::warn!(
                            "[media-probe] picture frame-count evidence unavailable: path={:?} stream={} reason={}",
                            path,
                            video.index,
                            reason
                        );
                }
            }
        }
    }

    for video in &mut video_streams {
        if !video_stream_needs_frame_hdr_probe(video) {
            continue;
        }
        match probe_first_frame_hdr_metadata(path, video.index) {
            Ok(frame_metadata) => {
                merge_hdr_metadata(&mut video.hdr_metadata, frame_metadata);
            }
            Err(reason) => {
                tracing::warn!(
                        "[media-probe] first-frame HDR metadata unavailable: path={:?} stream={} reason={}",
                        path,
                        video.index,
                        reason
                    );
            }
        }
    }

    let info = MediaProbeSnapshot {
        duration,
        file_size,
        container,
        has_video: !video_streams.is_empty(),
        has_audio: !audio_streams.is_empty(),
        video_streams,
        audio_streams,
    };

    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    if elapsed_ms >= media_probe_slow_threshold_ms() {
        tracing::warn!(
            "[media-probe] slow probe: {}ms path={:?} has_video={} has_audio={} streams(v/a)={}/{}",
            elapsed_ms,
            path,
            info.has_video,
            info.has_audio,
            info.video_streams.len(),
            info.audio_streams.len(),
        );
    } else {
        tracing::info!("[media-probe] done: {}ms path={:?}", elapsed_ms, path);
    }

    Ok(info)
}

#[cfg(test)]
fn detect_color_space(
    primaries: ffmpeg::util::color::Primaries,
    transfer: ffmpeg::util::color::TransferCharacteristic,
    matrix: ffmpeg::util::color::Space,
) -> DetectedColorInterpretation {
    let metadata = capture_color_metadata(primaries, transfer, matrix);
    let pixel_format = if matrix == ffmpeg::util::color::Space::RGB {
        PixelFormat::Rgb24
    } else {
        PixelFormat::Yuv420p
    };
    detect_color_space_from_metadata(
        &metadata,
        &[],
        None,
        Some(ProvenVideoSampling {
            pixel_format,
            bit_depth: pixel_format.bit_depth(),
            has_alpha: pixel_format.has_alpha(),
        }),
    )
}

fn detect_color_space_from_metadata(
    metadata: &VideoColorMetadata,
    metadata_hints: &[VideoColorMetadataHint],
    icc_profile: Option<&IccColorProfileHint>,
    sampling: Option<ProvenVideoSampling>,
) -> DetectedColorInterpretation {
    let mut canonical_metadata_hints = metadata_hints.to_vec();
    canonical_metadata_hints.sort_by(metadata_hint_diagnostic_order);
    let metadata_hints = canonical_metadata_hints.as_slice();
    let exact_cicp = metadata.exact_cicp_candidate_for_sampling(sampling);
    let hinted_cicp = ColorSpace::from_ffmpeg_tag_hints(
        metadata.primaries.name.as_deref(),
        metadata.transfer.name.as_deref(),
        metadata.matrix.name.as_deref(),
    );
    let cicp_color_space = exact_cicp.or(hinted_cicp);
    let unresolved_cicp = UnresolvedCicpMetadata::from_metadata(metadata);

    let declaration_resolution = resolve_video_color_metadata_declarations(metadata_hints);
    if let VideoColorMetadataDeclarationResolution::Conflicting { scope } = declaration_resolution {
        let Some((selected_index, selected_hint)) = metadata_hints
            .iter()
            .enumerate()
            .filter(|(_, hint)| hint.scope == scope && hint.is_executable_declaration())
            .min_by(|(_, left), (_, right)| metadata_hint_diagnostic_order(left, right))
        else {
            return unresolved_color_interpretation(
                metadata,
                metadata_hints,
                icc_profile,
                unresolved_cicp,
            );
        };
        let color_space = selected_hint.detected_color_space;
        let mut ignored = metadata_hints
            .iter()
            .enumerate()
            .filter(|(index, hint)| {
                *index != selected_index && hint.detected_color_space != color_space
            })
            .map(|(_, hint)| hint.clone())
            .collect::<Vec<_>>();
        ignored.sort_by(metadata_hint_diagnostic_order);
        let mut interpretation = DetectedColorInterpretation {
            candidate_color_space: Some(color_space),
            confidence: VideoColorInterpretationConfidence::Medium,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
            evidence: metadata_hints.iter().map(metadata_hint_evidence).collect(),
            warnings: vec![VideoColorInterpretationWarning::MultipleMetadataHints {
                selected: selected_hint.clone(),
                ignored,
            }],
            user_overridable: true,
        };
        append_icc_profile_evidence_and_warnings(
            &mut interpretation,
            icc_profile,
            cicp_color_space,
        );
        if cicp_color_space.is_none() && unresolved_cicp == UnresolvedCicpMetadata::Unsupported {
            unresolved_cicp.append_evidence(&mut interpretation.evidence, metadata);
            interpretation.warnings.push(unresolved_cicp.warning());
        }
        return interpretation;
    }

    if let VideoColorMetadataDeclarationResolution::Unique { scope, color_space } =
        declaration_resolution
    {
        let Some((selected_index, selected_hint)) = metadata_hints
            .iter()
            .enumerate()
            .filter(|(_, hint)| {
                hint.scope == scope
                    && hint.detected_color_space == color_space
                    && hint.is_executable_declaration()
            })
            .min_by(|(_, left), (_, right)| metadata_hint_diagnostic_order(left, right))
        else {
            return unresolved_color_interpretation(
                metadata,
                metadata_hints,
                icc_profile,
                unresolved_cicp,
            );
        };
        let color_space = selected_hint.detected_color_space;
        let mut interpretation = DetectedColorInterpretation {
            candidate_color_space: Some(color_space),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
            evidence: metadata_hints.iter().map(metadata_hint_evidence).collect(),
            warnings: Vec::new(),
            user_overridable: true,
        };
        append_icc_profile_evidence_and_warnings(
            &mut interpretation,
            icc_profile,
            cicp_color_space,
        );
        let mut conflicting_hints = metadata_hints
            .iter()
            .enumerate()
            .filter(|(index, hint)| {
                *index != selected_index && hint.detected_color_space != color_space
            })
            .map(|(_, hint)| hint.clone())
            .collect::<Vec<_>>();
        conflicting_hints.sort_by(metadata_hint_diagnostic_order);
        if !conflicting_hints.is_empty() {
            interpretation
                .warnings
                .push(VideoColorInterpretationWarning::MultipleMetadataHints {
                    selected: selected_hint.clone(),
                    ignored: conflicting_hints,
                });
        }
        if let Some(cicp_color_space) = cicp_color_space.filter(|cicp| *cicp != color_space) {
            interpretation.warnings.push(
                VideoColorInterpretationWarning::MetadataHintOverridesCicpTags {
                    selected: selected_hint.clone(),
                    cicp_color_space,
                    cicp_metadata: metadata.clone(),
                },
            );
        }
        if cicp_color_space.is_none() && unresolved_cicp == UnresolvedCicpMetadata::Unsupported {
            unresolved_cicp.append_evidence(&mut interpretation.evidence, metadata);
            interpretation.warnings.push(unresolved_cicp.warning());
        }
        return interpretation;
    }

    if let Some(color_space) = exact_cicp {
        let mut evidence = vec![VideoColorInterpretationEvidence::ExactCicpTags {
            primaries: metadata.primaries.clone(),
            transfer: metadata.transfer.clone(),
            matrix: metadata.matrix.clone(),
            detected_color_space: color_space,
        }];
        evidence.extend(metadata_hints.iter().map(metadata_hint_evidence));
        let mut interpretation = DetectedColorInterpretation {
            candidate_color_space: Some(color_space),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::CicpTags,
            evidence,
            warnings: Vec::new(),
            user_overridable: true,
        };
        append_icc_profile_evidence_and_warnings(
            &mut interpretation,
            icc_profile,
            Some(color_space),
        );
        if let Some(warning) = lower_priority_metadata_hints_warning(
            metadata_hints,
            VideoColorDetectionMethod::CicpTags,
            color_space,
        ) {
            interpretation.warnings.push(warning);
        }
        return interpretation;
    }

    if let Some(color_space) = hinted_cicp {
        let mut evidence = vec![VideoColorInterpretationEvidence::PartialCicpTags {
            primaries: metadata.primaries.clone(),
            transfer: metadata.transfer.clone(),
            matrix: metadata.matrix.clone(),
            detected_color_space: color_space,
        }];
        evidence.extend(metadata_hints.iter().map(metadata_hint_evidence));
        let mut interpretation = DetectedColorInterpretation {
            candidate_color_space: Some(color_space),
            confidence: VideoColorInterpretationConfidence::Medium,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::CicpTags,
            evidence,
            warnings: vec![VideoColorInterpretationWarning::PartialCicpTags {
                detected_color_space: color_space,
            }],
            user_overridable: true,
        };
        append_icc_profile_evidence_and_warnings(
            &mut interpretation,
            icc_profile,
            Some(color_space),
        );
        if let Some(warning) = lower_priority_metadata_hints_warning(
            metadata_hints,
            VideoColorDetectionMethod::CicpTags,
            color_space,
        ) {
            interpretation.warnings.push(warning);
        }
        return interpretation;
    }

    if let Some(icc_profile) = icc_profile {
        if let Some(color_space) = icc_profile.mapping.color_space() {
            let mut evidence = vec![VideoColorInterpretationEvidence::IccProfile {
                mapped_color_space: Some(color_space),
                profile_name: icc_profile.profile_name.clone(),
            }];
            unresolved_cicp.append_evidence(&mut evidence, metadata);
            evidence.extend(metadata_hints.iter().map(metadata_hint_evidence));
            let mut interpretation = DetectedColorInterpretation {
                candidate_color_space: Some(color_space),
                confidence: VideoColorInterpretationConfidence::Medium,
                source: VideoColorSpaceSource::Metadata,
                method: VideoColorDetectionMethod::IccProfile,
                evidence,
                warnings: vec![unresolved_cicp.warning()],
                user_overridable: true,
            };
            if let Some(warning) = lower_priority_metadata_hints_warning(
                metadata_hints,
                VideoColorDetectionMethod::IccProfile,
                color_space,
            ) {
                interpretation.warnings.push(warning);
            }
            return interpretation;
        }
        if metadata_hints.is_empty() {
            let mut evidence = vec![icc_profile_evidence(icc_profile)];
            unresolved_cicp.append_evidence(&mut evidence, metadata);
            let mut interpretation = DetectedColorInterpretation {
                candidate_color_space: None,
                confidence: VideoColorInterpretationConfidence::None,
                source: unresolved_cicp.source(),
                method: unresolved_cicp.method(),
                evidence,
                warnings: vec![unresolved_cicp.warning()],
                user_overridable: true,
            };
            append_unmapped_icc_warning(&mut interpretation, icc_profile);
            return interpretation;
        }
    }

    if let Some((_, selected_hint)) = metadata_hints
        .iter()
        .enumerate()
        .min_by_key(|(index, hint)| (metadata_hint_scope_priority(hint.scope), *index))
    {
        return descriptive_metadata_hint_interpretation(
            selected_hint,
            metadata_hints,
            icc_profile,
            metadata,
            unresolved_cicp,
        );
    }

    unresolved_color_interpretation(metadata, metadata_hints, icc_profile, unresolved_cicp)
}

fn metadata_hint_diagnostic_order(
    left: &VideoColorMetadataHint,
    right: &VideoColorMetadataHint,
) -> std::cmp::Ordering {
    metadata_hint_scope_priority(left.scope)
        .cmp(&metadata_hint_scope_priority(right.scope))
        .then_with(|| left.key.cmp(&right.key))
        .then_with(|| left.value.cmp(&right.value))
        .then_with(|| (left.detected_color_space as u8).cmp(&(right.detected_color_space as u8)))
}

fn unresolved_color_interpretation(
    metadata: &VideoColorMetadata,
    metadata_hints: &[VideoColorMetadataHint],
    icc_profile: Option<&IccColorProfileHint>,
    unresolved_cicp: UnresolvedCicpMetadata,
) -> DetectedColorInterpretation {
    let mut evidence = metadata_hints.iter().map(metadata_hint_evidence).collect::<Vec<_>>();
    unresolved_cicp.append_evidence(&mut evidence, metadata);
    let mut interpretation = DetectedColorInterpretation {
        candidate_color_space: None,
        confidence: VideoColorInterpretationConfidence::None,
        source: unresolved_cicp.source(),
        method: unresolved_cicp.method(),
        evidence,
        warnings: vec![unresolved_cicp.warning()],
        user_overridable: true,
    };
    append_icc_profile_evidence_and_warnings(&mut interpretation, icc_profile, None);
    interpretation
}

fn descriptive_metadata_hint_interpretation(
    selected_hint: &VideoColorMetadataHint,
    metadata_hints: &[VideoColorMetadataHint],
    icc_profile: Option<&IccColorProfileHint>,
    metadata: &VideoColorMetadata,
    unresolved_cicp: UnresolvedCicpMetadata,
) -> DetectedColorInterpretation {
    let mut evidence = metadata_hints.iter().map(metadata_hint_evidence).collect::<Vec<_>>();
    unresolved_cicp.append_evidence(&mut evidence, metadata);
    let mut interpretation = DetectedColorInterpretation {
        candidate_color_space: Some(selected_hint.detected_color_space),
        confidence: VideoColorInterpretationConfidence::Low,
        source: VideoColorSpaceSource::Metadata,
        method: VideoColorDetectionMethod::MetadataHint,
        evidence,
        warnings: vec![
            VideoColorInterpretationWarning::DescriptiveMetadataHintInference {
                selected: selected_hint.clone(),
            },
            unresolved_cicp.warning(),
        ],
        user_overridable: true,
    };
    let conflicting_hints = metadata_hints
        .iter()
        .filter(|hint| {
            !std::ptr::eq(*hint, selected_hint)
                && hint.detected_color_space != selected_hint.detected_color_space
        })
        .cloned()
        .collect::<Vec<_>>();
    if !conflicting_hints.is_empty() {
        interpretation
            .warnings
            .push(VideoColorInterpretationWarning::MultipleMetadataHints {
                selected: selected_hint.clone(),
                ignored: conflicting_hints,
            });
    }
    append_icc_profile_evidence_and_warnings(
        &mut interpretation,
        icc_profile,
        Some(selected_hint.detected_color_space),
    );
    interpretation
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnresolvedCicpMetadata {
    Missing,
    Unsupported,
}

impl UnresolvedCicpMetadata {
    fn from_metadata(metadata: &VideoColorMetadata) -> Self {
        if metadata.primaries.specified || metadata.transfer.specified || metadata.matrix.specified
        {
            Self::Unsupported
        } else {
            Self::Missing
        }
    }

    fn source(self) -> VideoColorSpaceSource {
        match self {
            Self::Missing => VideoColorSpaceSource::MissingMetadata,
            Self::Unsupported => VideoColorSpaceSource::UnsupportedMetadata,
        }
    }

    fn method(self) -> VideoColorDetectionMethod {
        match self {
            Self::Missing => VideoColorDetectionMethod::MissingMetadata,
            Self::Unsupported => VideoColorDetectionMethod::UnsupportedCicpTags,
        }
    }

    fn warning(self) -> VideoColorInterpretationWarning {
        match self {
            Self::Missing => VideoColorInterpretationWarning::MissingCicpTags,
            Self::Unsupported => VideoColorInterpretationWarning::UnsupportedCicpTags,
        }
    }

    fn append_evidence(
        self,
        evidence: &mut Vec<VideoColorInterpretationEvidence>,
        metadata: &VideoColorMetadata,
    ) {
        if self == Self::Unsupported {
            evidence.push(VideoColorInterpretationEvidence::UnsupportedCicpTags {
                primaries: metadata.primaries.clone(),
                transfer: metadata.transfer.clone(),
                matrix: metadata.matrix.clone(),
            });
        }
    }
}

/// Interpret structured video color metadata without applying project policy.
///
/// This entry point is shared by decoder integrations that already captured
/// CICP tags, proven physical sampling when available, and acquisition metadata
/// hints. Missing or unproven facts remain unresolved; the timeline's
/// missing-metadata policy owns any later assumption.
pub fn interpret_video_color_metadata(
    metadata: &VideoColorMetadata,
    sampling: Option<ProvenVideoSampling>,
    metadata_hints: &[VideoColorMetadataHint],
) -> DetectedColorInterpretation {
    detect_color_space_from_metadata(metadata, metadata_hints, None, sampling)
}

fn append_icc_profile_evidence_and_warnings(
    interpretation: &mut DetectedColorInterpretation,
    icc_profile: Option<&IccColorProfileHint>,
    selected_color_space: Option<ColorSpace>,
) {
    let Some(icc_profile) = icc_profile else {
        return;
    };
    interpretation.evidence.push(icc_profile_evidence(icc_profile));
    let Some(icc_color_space) = icc_profile.mapping.color_space() else {
        append_unmapped_icc_warning(interpretation, icc_profile);
        return;
    };
    if let Some(selected_color_space) =
        selected_color_space.filter(|space| *space != icc_color_space)
    {
        interpretation.warnings.push(VideoColorInterpretationWarning::IccCicpMismatch {
            icc_color_space,
            cicp_color_space: selected_color_space,
        });
    }
}

fn icc_profile_evidence(icc_profile: &IccColorProfileHint) -> VideoColorInterpretationEvidence {
    VideoColorInterpretationEvidence::IccProfile {
        mapped_color_space: icc_profile.mapping.color_space(),
        profile_name: icc_profile.profile_name.clone(),
    }
}

fn append_unmapped_icc_warning(
    interpretation: &mut DetectedColorInterpretation,
    icc_profile: &IccColorProfileHint,
) {
    let mondrian_core::icc::IccColorSpaceMapping::Unmapped { reason, .. } = &icc_profile.mapping
    else {
        return;
    };
    interpretation
        .warnings
        .push(VideoColorInterpretationWarning::IccProfileUnmapped {
            profile_name: icc_profile.profile_name.clone(),
            reason: reason.clone(),
        });
}

fn metadata_hint_evidence(hint: &VideoColorMetadataHint) -> VideoColorInterpretationEvidence {
    VideoColorInterpretationEvidence::MetadataHint {
        scope: hint.scope,
        key: hint.key.clone(),
        value: hint.value.clone(),
        detected_color_space: hint.detected_color_space,
        authority: hint.authority,
    }
}

fn lower_priority_metadata_hints_warning(
    metadata_hints: &[VideoColorMetadataHint],
    selected_method: VideoColorDetectionMethod,
    selected_color_space: ColorSpace,
) -> Option<VideoColorInterpretationWarning> {
    let ignored = metadata_hints
        .iter()
        .filter(|hint| hint.detected_color_space != selected_color_space)
        .cloned()
        .collect::<Vec<_>>();
    (!ignored.is_empty()).then_some(
        VideoColorInterpretationWarning::LowerPriorityMetadataHints {
            selected_method,
            selected_color_space,
            ignored,
        },
    )
}

fn metadata_hint_scope_priority(scope: VideoColorMetadataHintScope) -> u8 {
    match scope {
        VideoColorMetadataHintScope::Stream => 0,
        VideoColorMetadataHintScope::Container => 1,
        VideoColorMetadataHintScope::FileName => 2,
    }
}

fn collect_color_metadata_hints(
    scope: VideoColorMetadataHintScope,
    metadata: &ffmpeg::DictionaryRef<'_>,
) -> Vec<VideoColorMetadataHint> {
    metadata
        .iter()
        .filter_map(|(key, value)| parse_video_color_metadata_hint(scope, key, value))
        .collect()
}

/// Parse one container, stream, or file-name field as a complete color-space hint.
///
/// Ambiguous curve-only or gamut-only text remains unresolved because a
/// professional acquisition identity requires both transfer and primaries.
pub fn parse_video_color_metadata_hint(
    scope: VideoColorMetadataHintScope,
    key: &str,
    value: &str,
) -> Option<VideoColorMetadataHint> {
    let haystack = normalize_metadata_hint_text(&format!("{key} {value}"));
    let detected_color_space = if contains_any(&haystack, &["aces20651", "aces2065ap0"]) {
        Some(ColorSpace::Aces2065_1)
    } else if contains_any(&haystack, &["acescct", "acescctap1"]) {
        Some(ColorSpace::AcesCct)
    } else if contains_any(&haystack, &["acescg", "acescgap1"]) {
        Some(ColorSpace::AcesCg)
    } else if contains_any(&haystack, &["linearrec2020", "linrec2020"]) {
        Some(ColorSpace::LinearRec2020)
    } else if contains_any(&haystack, &["linearrec709", "linrec709", "linearsrgb"]) {
        Some(ColorSpace::LinearRec709)
    } else if contains_any(&haystack, &["linearp3d65", "linp3d65"]) {
        Some(ColorSpace::LinearP3D65)
    } else if contains_any(&haystack, &["slog2sgamut", "sonyslog2sgamut"]) {
        Some(ColorSpace::SonySLog2SGamut)
    } else if contains_any(&haystack, &["applelog", "applelogprofile"]) {
        Some(ColorSpace::AppleLogBt2020)
    } else if contains_any(&haystack, &["slog3sgamut3cine", "sonyslog3sgamut3cine"]) {
        Some(ColorSpace::SonySLog3SGamut3Cine)
    } else if contains_any(&haystack, &["slog3sgamut3", "sonyslog3sgamut3"]) {
        Some(ColorSpace::SonySLog3SGamut3)
    } else if contains_any(&haystack, &["arrilogc4", "logc4widegamut4", "logc4awg4"]) {
        Some(ColorSpace::ArriLogC4WideGamut4)
    } else if contains_any(
        &haystack,
        &["arrilogc3", "logc3widegamut3", "logc3awg3", "logc3ei800"],
    ) {
        Some(ColorSpace::ArriLogC3WideGamut3)
    } else if contains_any(&haystack, &["canonlog2cinemagamut", "clog2cinemagamut"]) {
        Some(ColorSpace::CanonLog2CinemaGamutD55)
    } else if contains_any(&haystack, &["canonlog3cinemagamut", "clog3cinemagamut"]) {
        Some(ColorSpace::CanonLog3CinemaGamutD55)
    } else if contains_any(&haystack, &["vlogvgamut", "panasonicvlogvgamut"]) {
        Some(ColorSpace::PanasonicVLogVGamut)
    } else if contains_any(&haystack, &["log3g10redwidegamutrgb", "redlog3g10rwg"]) {
        Some(ColorSpace::RedLog3G10WideGamutRgb)
    } else if contains_any(
        &haystack,
        &["bmdfilmwidegamutgen5", "blackmagicfilmwidegamutgen5"],
    ) {
        Some(ColorSpace::BlackmagicFilmWideGamutGen5)
    } else if contains_any(&haystack, &["dlogdgamut", "djidlogdgamut"]) {
        Some(ColorSpace::DjiDLogDGamut)
    } else if contains_any(
        &haystack,
        &["davinciintermediatewidegamut", "davinciintermediatedwg"],
    ) {
        Some(ColorSpace::DavinciIntermediateWideGamut)
    } else {
        None
    }?;

    Some(VideoColorMetadataHint {
        scope,
        key: key.to_string(),
        value: value.to_string(),
        detected_color_space,
        authority: metadata_hint_authority(scope, key),
    })
}

fn metadata_hint_authority(
    scope: VideoColorMetadataHintScope,
    key: &str,
) -> VideoColorMetadataHintAuthority {
    let declaration = if key.eq_ignore_ascii_case("com.sony.colorProfile") {
        Some(VideoColorMetadataDeclaration::SonyColorProfile)
    } else if key.eq_ignore_ascii_case("com.apple.proapps.cameraLog") {
        Some(VideoColorMetadataDeclaration::AppleProAppsCameraLog)
    } else {
        None
    };

    match (scope, declaration) {
        (
            VideoColorMetadataHintScope::Container | VideoColorMetadataHintScope::Stream,
            Some(declaration),
        ) => VideoColorMetadataHintAuthority::SourceDeclaration(declaration),
        _ => VideoColorMetadataHintAuthority::DiagnosticSuggestion,
    }
}

fn normalize_metadata_hint_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn collect_hdr_metadata_summaries(
    stream: &ffmpeg::format::stream::Stream<'_>,
) -> Vec<VideoHdrMetadataSummary> {
    stream
        .side_data()
        .filter_map(|side_data| {
            let side_data_kind = side_data.kind();
            map_hdr_side_data_kind(side_data_kind).map(|kind| VideoHdrMetadataSummary {
                kind,
                payload_size: side_data.data().len(),
                payload: parse_hdr_metadata_payload(side_data_kind, side_data.data()),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PictureFrameDrain {
    NeedInput,
    EndOfStream,
    MultipleFrames,
}

fn drain_picture_probe_frames(
    decoder: &mut ffmpeg::decoder::Video,
    frame_count: &mut usize,
) -> Result<PictureFrameDrain, String> {
    loop {
        let mut decoded = ffmpeg::util::frame::video::Video::empty();
        match decoder.receive_frame(&mut decoded) {
            Ok(()) => {
                *frame_count = frame_count.saturating_add(1);
                if *frame_count >= 2 {
                    return Ok(PictureFrameDrain::MultipleFrames);
                }
            }
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                return Ok(PictureFrameDrain::NeedInput);
            }
            Err(ffmpeg::Error::Eof) => return Ok(PictureFrameDrain::EndOfStream),
            Err(error) => return Err(format!("receive picture frame: {error}")),
        }
    }
}

fn probe_exactly_one_video_frame(path: &Path, stream_index: u32) -> Result<bool, String> {
    const MAX_TARGET_PACKETS: usize = 512;

    let mut input =
        ffmpeg::format::input(path).map_err(|error| format!("open picture probe: {error}"))?;
    let parameters = input
        .streams()
        .find(|stream| stream.index() == stream_index as usize)
        .map(|stream| stream.parameters())
        .ok_or_else(|| format!("video stream {stream_index} is unavailable"))?;
    let context = ffmpeg::codec::context::Context::from_parameters(parameters)
        .map_err(|error| format!("create picture decoder context: {error}"))?;
    let mut decoder = context
        .decoder()
        .video()
        .map_err(|error| format!("open picture decoder: {error}"))?;
    let mut frame_count = 0usize;
    let mut packet_count = 0usize;

    for (stream, packet) in input.packets() {
        if stream.index() != stream_index as usize {
            continue;
        }
        packet_count = packet_count.saturating_add(1);
        if packet_count > MAX_TARGET_PACKETS {
            return Err(format!(
                "picture probe exceeded {MAX_TARGET_PACKETS} target packets"
            ));
        }
        decoder
            .send_packet(&packet)
            .map_err(|error| format!("send picture packet: {error}"))?;
        match drain_picture_probe_frames(&mut decoder, &mut frame_count)? {
            PictureFrameDrain::MultipleFrames => return Ok(false),
            PictureFrameDrain::EndOfStream => return Ok(frame_count == 1),
            PictureFrameDrain::NeedInput => {}
        }
    }

    decoder.send_eof().map_err(|error| format!("flush picture decoder: {error}"))?;
    match drain_picture_probe_frames(&mut decoder, &mut frame_count)? {
        PictureFrameDrain::MultipleFrames => Ok(false),
        PictureFrameDrain::EndOfStream => Ok(frame_count == 1),
        PictureFrameDrain::NeedInput => Err("picture decoder requested input after EOF".to_owned()),
    }
}

fn video_stream_needs_frame_hdr_probe(stream: &VideoStreamInfo) -> bool {
    stream.executable_color_space().is_some_and(ColorSpace::is_hdr)
        || stream.hdr_metadata.iter().any(|metadata| {
            matches!(
                metadata.kind,
                VideoHdrSideDataKind::DynamicHdr10Plus | VideoHdrSideDataKind::DolbyVisionConfig
            )
        })
}

fn probe_first_frame_hdr_metadata(
    path: &Path,
    stream_index: u32,
) -> Result<Vec<VideoHdrMetadataSummary>, String> {
    const MAX_VIDEO_PACKETS: usize = 512;

    let mut input = ffmpeg::format::input(path)
        .map_err(|error| format!("open first-frame HDR probe input: {error}"))?;
    let parameters = input
        .streams()
        .find(|stream| stream.index() == stream_index as usize)
        .map(|stream| stream.parameters())
        .ok_or_else(|| format!("video stream {stream_index} is unavailable"))?;
    let context = ffmpeg::codec::context::Context::from_parameters(parameters)
        .map_err(|error| format!("create first-frame HDR decoder context: {error}"))?;
    let mut decoder = context
        .decoder()
        .video()
        .map_err(|error| format!("open first-frame HDR video decoder: {error}"))?;

    let mut target_packets = 0usize;
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index as usize {
            continue;
        }
        target_packets = target_packets.saturating_add(1);
        decoder
            .send_packet(&packet)
            .map_err(|error| format!("send first-frame HDR packet: {error}"))?;
        let mut decoded = ffmpeg::util::frame::video::Video::empty();
        if decoder.receive_frame(&mut decoded).is_ok() {
            return Ok(collect_frame_hdr_metadata_summaries(&decoded));
        }
        if target_packets >= MAX_VIDEO_PACKETS {
            return Err(format!(
                "no decoded frame after {MAX_VIDEO_PACKETS} packets"
            ));
        }
    }

    decoder
        .send_eof()
        .map_err(|error| format!("flush first-frame HDR decoder: {error}"))?;
    let mut decoded = ffmpeg::util::frame::video::Video::empty();
    decoder
        .receive_frame(&mut decoded)
        .map_err(|error| format!("decode first HDR frame at end of stream: {error}"))?;
    Ok(collect_frame_hdr_metadata_summaries(&decoded))
}

fn collect_frame_hdr_metadata_summaries(
    frame: &ffmpeg::util::frame::video::Video,
) -> Vec<VideoHdrMetadataSummary> {
    use ffmpeg::util::frame::side_data::Type;

    [
        (
            Type::MasteringDisplayMetadata,
            VideoHdrSideDataKind::MasteringDisplayMetadata,
        ),
        (
            Type::ContentLightLevel,
            VideoHdrSideDataKind::ContentLightLevel,
        ),
        (
            Type::DYNAMIC_HDR_PLUS,
            VideoHdrSideDataKind::DynamicHdr10Plus,
        ),
    ]
    .into_iter()
    .filter_map(|(side_data_type, kind)| {
        let side_data = frame.side_data(side_data_type)?;
        Some(VideoHdrMetadataSummary {
            kind,
            payload_size: side_data.data().len(),
            payload: parse_hdr_metadata_payload_for_kind(kind, side_data.data()),
        })
    })
    .collect()
}

fn merge_hdr_metadata(
    stream_metadata: &mut Vec<VideoHdrMetadataSummary>,
    frame_metadata: Vec<VideoHdrMetadataSummary>,
) {
    for candidate in frame_metadata {
        if let Some(existing) =
            stream_metadata.iter_mut().find(|metadata| metadata.kind == candidate.kind)
        {
            if existing.payload.is_none() && candidate.payload.is_some() {
                *existing = candidate;
            }
        } else {
            stream_metadata.push(candidate);
        }
    }
}

fn icc_color_profile_hint(hdr_metadata: &[VideoHdrMetadataSummary]) -> Option<IccColorProfileHint> {
    hdr_metadata.iter().find_map(|summary| {
        let VideoHdrMetadataPayload::IccProfile(profile) = summary.payload.as_ref()? else {
            return None;
        };
        Some(IccColorProfileHint {
            mapping: profile.mapping.clone(),
            profile_name: Some(profile.name.clone()),
        })
    })
}

fn map_hdr_side_data_kind(
    kind: ffmpeg::codec::packet::side_data::Type,
) -> Option<VideoHdrSideDataKind> {
    use ffmpeg::codec::packet::side_data::Type;

    match kind {
        Type::MasteringDisplayMetadata => Some(VideoHdrSideDataKind::MasteringDisplayMetadata),
        Type::ContentLightLevel => Some(VideoHdrSideDataKind::ContentLightLevel),
        Type::DYNAMIC_HDR10_PLUS => Some(VideoHdrSideDataKind::DynamicHdr10Plus),
        Type::DOVI_CONF => Some(VideoHdrSideDataKind::DolbyVisionConfig),
        Type::ICC_PROFILE => Some(VideoHdrSideDataKind::IccProfile),
        _ => None,
    }
}

fn parse_hdr_metadata_payload(
    kind: ffmpeg::codec::packet::side_data::Type,
    data: &[u8],
) -> Option<VideoHdrMetadataPayload> {
    let summary_kind = map_hdr_side_data_kind(kind)?;
    if summary_kind == VideoHdrSideDataKind::IccProfile {
        return parse_icc_display_profile(data)
            .ok()
            .map(|profile| VideoIccProfileMetadata { name: profile.name, mapping: profile.mapping })
            .map(VideoHdrMetadataPayload::IccProfile);
    }
    parse_hdr_metadata_payload_for_kind(summary_kind, data)
}

fn parse_hdr_metadata_payload_for_kind(
    kind: VideoHdrSideDataKind,
    data: &[u8],
) -> Option<VideoHdrMetadataPayload> {
    match kind {
        VideoHdrSideDataKind::MasteringDisplayMetadata => {
            parse_mastering_display_payload(data).map(VideoHdrMetadataPayload::MasteringDisplay)
        }
        VideoHdrSideDataKind::ContentLightLevel => {
            parse_content_light_payload(data).map(VideoHdrMetadataPayload::ContentLightLevel)
        }
        VideoHdrSideDataKind::DynamicHdr10Plus
        | VideoHdrSideDataKind::DolbyVisionConfig
        | VideoHdrSideDataKind::IccProfile => None,
    }
}

fn parse_mastering_display_payload(data: &[u8]) -> Option<VideoMasteringDisplayMetadata> {
    let raw = read_unaligned_payload::<FfmpegMasteringDisplayMetadata>(data)?;
    let primaries = (raw.has_primaries != 0).then(|| VideoMasteringDisplayPrimaries {
        red: chromaticity_from_ffmpeg(raw.display_primaries[0]),
        green: chromaticity_from_ffmpeg(raw.display_primaries[1]),
        blue: chromaticity_from_ffmpeg(raw.display_primaries[2]),
        white_point: chromaticity_from_ffmpeg(raw.white_point),
    });
    let luminance = (raw.has_luminance != 0).then(|| VideoMasteringDisplayLuminance {
        min: rational_from_ffmpeg(raw.min_luminance),
        max: rational_from_ffmpeg(raw.max_luminance),
    });
    Some(VideoMasteringDisplayMetadata { primaries, luminance })
}

fn parse_content_light_payload(data: &[u8]) -> Option<VideoContentLightMetadata> {
    let raw = read_unaligned_payload::<FfmpegContentLightMetadata>(data)?;
    Some(VideoContentLightMetadata {
        max_content_light_level: raw.max_cll,
        max_frame_average_light_level: raw.max_fall,
    })
}

fn read_unaligned_payload<T: Copy>(data: &[u8]) -> Option<T> {
    if data.len() < std::mem::size_of::<T>() {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<T>()) })
}

fn chromaticity_from_ffmpeg(raw: [FfmpegRational; 2]) -> VideoHdrChromaticity {
    VideoHdrChromaticity {
        x: rational_from_ffmpeg(raw[0]),
        y: rational_from_ffmpeg(raw[1]),
    }
}

fn rational_from_ffmpeg(raw: FfmpegRational) -> VideoHdrRational {
    VideoHdrRational::new(raw.num, raw.den)
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct FfmpegRational {
    num: c_int,
    den: c_int,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct FfmpegMasteringDisplayMetadata {
    display_primaries: [[FfmpegRational; 2]; 3],
    white_point: [FfmpegRational; 2],
    min_luminance: FfmpegRational,
    max_luminance: FfmpegRational,
    has_primaries: c_int,
    has_luminance: c_int,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct FfmpegContentLightMetadata {
    max_cll: u32,
    max_fall: u32,
}

fn capture_color_metadata(
    primaries: ffmpeg::util::color::Primaries,
    transfer: ffmpeg::util::color::TransferCharacteristic,
    matrix: ffmpeg::util::color::Space,
) -> VideoColorMetadata {
    VideoColorMetadata {
        primaries: primaries_tag(primaries),
        transfer: transfer_tag(transfer),
        matrix: matrix_tag(matrix),
    }
}

fn primaries_tag(value: ffmpeg::util::color::Primaries) -> VideoColorTag {
    let raw: ffmpeg::ffi::AVColorPrimaries = value.into();
    VideoColorTag {
        code: raw as i32,
        name: value.name().map(str::to_owned),
        specified: value != ffmpeg::util::color::Primaries::Unspecified,
    }
}

fn transfer_tag(value: ffmpeg::util::color::TransferCharacteristic) -> VideoColorTag {
    let raw: ffmpeg::ffi::AVColorTransferCharacteristic = value.into();
    VideoColorTag {
        code: raw as i32,
        name: value.name().map(str::to_owned),
        specified: value != ffmpeg::util::color::TransferCharacteristic::Unspecified,
    }
}

fn matrix_tag(value: ffmpeg::util::color::Space) -> VideoColorTag {
    let raw: ffmpeg::ffi::AVColorSpace = value.into();
    VideoColorTag {
        code: raw as i32,
        name: value.name().map(str::to_owned),
        specified: value != ffmpeg::util::color::Space::Unspecified,
    }
}

fn map_rational(value: ffmpeg::Rational) -> (Rational, bool) {
    let num = value.numerator();
    let den = value.denominator();
    if num <= 0 || den <= 0 {
        (Rational::new(0, 1), false)
    } else {
        (
            canonicalize_frame_rate(Rational::new(num as i64, den as i64)),
            true,
        )
    }
}

fn duration_from_stream_ticks(ticks: i64, time_base: ffmpeg::Rational) -> Option<Duration> {
    let numerator = time_base.numerator();
    let denominator = time_base.denominator();
    if ticks <= 0 || numerator <= 0 || denominator <= 0 {
        return None;
    }
    let nanos = (ticks as u128)
        .checked_mul(numerator as u128)?
        .checked_mul(1_000_000_000)?
        .checked_div(denominator as u128)?;
    Some(Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64))
}

fn canonicalize_frame_rate(frame_rate: Rational) -> Rational {
    const TOLERANCE_PPM: i128 = 100;
    const NOMINAL_RATES: [Rational; 12] = [
        Rational::FPS_10,
        Rational::FPS_12,
        Rational::FPS_125,
        Rational::FPS_15,
        Rational::FPS_23976,
        Rational::FPS_24,
        Rational::FPS_25,
        Rational::FPS_2997,
        Rational::FPS_30,
        Rational::FPS_50,
        Rational::FPS_5994,
        Rational::FPS_60,
    ];

    NOMINAL_RATES
        .into_iter()
        .find(|nominal| {
            let cross_error = ((frame_rate.num as i128) * (nominal.den as i128)
                - (nominal.num as i128) * (frame_rate.den as i128))
                .abs();
            let nominal_cross = (nominal.num as i128).abs() * (frame_rate.den as i128).abs();
            cross_error.saturating_mul(1_000_000) <= nominal_cross.saturating_mul(TOLERANCE_PPM)
        })
        .unwrap_or(frame_rate)
}

fn map_pixel_format(pixel: ffmpeg::util::format::pixel::Pixel) -> Option<PixelFormat> {
    use ffmpeg::util::format::pixel::Pixel;

    match pixel {
        Pixel::YUV420P => Some(PixelFormat::Yuv420p),
        Pixel::YUV422P => Some(PixelFormat::Yuv422p),
        Pixel::YUV444P => Some(PixelFormat::Yuv444p),
        Pixel::YUV420P10LE => Some(PixelFormat::Yuv420p10le),
        Pixel::YUV422P10LE => Some(PixelFormat::Yuv422p10le),
        Pixel::YUV444P10LE => Some(PixelFormat::Yuv444p10le),
        Pixel::RGB24 => Some(PixelFormat::Rgb24),
        Pixel::RGBA => Some(PixelFormat::Rgba),
        Pixel::NV12 => Some(PixelFormat::Nv12),
        Pixel::P010LE => Some(PixelFormat::P010),
        _ => None,
    }
}

fn map_video_codec_profile(profile: ffmpeg::codec::Profile) -> VideoCodecProfile {
    use ffmpeg::codec::profile::{H264, HEVC};
    use ffmpeg::codec::Profile;

    match profile {
        Profile::Unknown | Profile::Reserved => VideoCodecProfile::Unknown,
        Profile::H264(H264::Constrained) => VideoCodecProfile::H264Constrained,
        Profile::H264(H264::Intra) => VideoCodecProfile::H264Intra,
        Profile::H264(H264::Baseline) => VideoCodecProfile::H264Baseline,
        Profile::H264(H264::ConstrainedBaseline) => VideoCodecProfile::H264ConstrainedBaseline,
        Profile::H264(H264::Main) => VideoCodecProfile::H264Main,
        Profile::H264(H264::Extended) => VideoCodecProfile::H264Extended,
        Profile::H264(H264::High) => VideoCodecProfile::H264High,
        Profile::H264(H264::High10) => VideoCodecProfile::H264High10,
        Profile::H264(H264::High10Intra) => VideoCodecProfile::H264High10Intra,
        Profile::H264(H264::High422) => VideoCodecProfile::H264High422,
        Profile::H264(H264::High422Intra) => VideoCodecProfile::H264High422Intra,
        Profile::H264(H264::High444) => VideoCodecProfile::H264High444,
        Profile::H264(H264::High444Predictive) => VideoCodecProfile::H264High444Predictive,
        Profile::H264(H264::High444Intra) => VideoCodecProfile::H264High444Intra,
        Profile::H264(H264::CAVLC444) => VideoCodecProfile::H264Cavlc444,
        Profile::HEVC(HEVC::Main) => VideoCodecProfile::HevcMain,
        Profile::HEVC(HEVC::Main10) => VideoCodecProfile::HevcMain10,
        Profile::HEVC(HEVC::MainStillPicture) => VideoCodecProfile::HevcMainStillPicture,
        Profile::HEVC(HEVC::Rext) => VideoCodecProfile::HevcRangeExtensions,
        _ => VideoCodecProfile::Other,
    }
}

fn map_channel_layout(layout: ffmpeg::ChannelLayout, reported_channels: u8) -> ChannelLayout {
    if layout.is_empty() {
        ChannelLayout::Unspecified(reported_channels)
    } else if layout.0.order != ffmpeg::ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE {
        ChannelLayout::Unsupported(reported_channels)
    } else {
        exact_signal_layout_from_ffmpeg_mask(layout.bits(), reported_channels).map_or(
            ChannelLayout::Unsupported(reported_channels),
            ChannelLayout::Exact,
        )
    }
}

fn exact_signal_layout_from_ffmpeg_mask(
    mask: u64,
    reported_channels: u8,
) -> Option<AudioChannelLayout> {
    // AVChannel native-mask positions are ABI-stable bit indexes. Keep this
    // translation explicit: core signal-layout bits are deliberately private
    // and must never be treated as FFmpeg masks.
    const POSITIONS: &[(u32, AudioChannelPosition)] = &[
        (0, AudioChannelPosition::FrontLeft),
        (1, AudioChannelPosition::FrontRight),
        (2, AudioChannelPosition::FrontCenter),
        (3, AudioChannelPosition::LowFrequencyEffects),
        (4, AudioChannelPosition::BackLeft),
        (5, AudioChannelPosition::BackRight),
        (6, AudioChannelPosition::FrontLeftOfCenter),
        (7, AudioChannelPosition::FrontRightOfCenter),
        (8, AudioChannelPosition::BackCenter),
        (9, AudioChannelPosition::SideLeft),
        (10, AudioChannelPosition::SideRight),
        (11, AudioChannelPosition::TopCenter),
        (12, AudioChannelPosition::TopFrontLeft),
        (13, AudioChannelPosition::TopFrontCenter),
        (14, AudioChannelPosition::TopFrontRight),
        (15, AudioChannelPosition::TopBackLeft),
        (16, AudioChannelPosition::TopBackCenter),
        (17, AudioChannelPosition::TopBackRight),
        (31, AudioChannelPosition::WideLeft),
        (32, AudioChannelPosition::WideRight),
        (35, AudioChannelPosition::LowFrequencyEffects2),
        (36, AudioChannelPosition::TopSideLeft),
        (37, AudioChannelPosition::TopSideRight),
    ];
    if reported_channels == 0 || u32::from(reported_channels) != mask.count_ones() {
        return None;
    }
    // A single FC channel is FFmpeg's native representation of Mono. It is
    // intentionally not a one-speaker set in Mondrian's signal contract.
    if mask == (1_u64 << 2) {
        return Some(AudioChannelLayout::Mono);
    }
    let supported_mask = POSITIONS.iter().fold(0_u64, |bits, (index, _)| bits | (1_u64 << index));
    if mask == 0 || mask & !supported_mask != 0 {
        return None;
    }
    AudioChannelLayout::speakers(
        POSITIONS
            .iter()
            .filter_map(|(index, position)| (mask & (1_u64 << index) != 0).then_some(*position)),
    )
    .ok()
}

fn normalized_stream_metadata(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned)
}

fn map_video_codec(id: ffmpeg::codec::Id) -> VideoCodec {
    use ffmpeg::codec::Id;

    match id {
        Id::H264 => VideoCodec::H264,
        Id::HEVC => VideoCodec::H265,
        Id::AV1 => VideoCodec::Av1,
        Id::VP9 => VideoCodec::Vp9,
        Id::PRORES => VideoCodec::ProRes(ProResVariant::Standard),
        Id::DNXHD => VideoCodec::DnxHd,
        Id::CFHD => VideoCodec::Cineform,
        Id::RAWVIDEO => VideoCodec::Raw,
        other => VideoCodec::Other(format!("{other:?}")),
    }
}

fn media_probe_slow_threshold_ms() -> u64 {
    static THRESHOLD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("MONDRIAN_MEDIA_PROBE_SLOW_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value >= 10)
            .unwrap_or(120)
    })
}

fn map_audio_codec(id: ffmpeg::codec::Id) -> AudioCodec {
    use ffmpeg::codec::Id;

    match id {
        Id::AAC => AudioCodec::Aac,
        Id::MP3 => AudioCodec::Mp3,
        Id::FLAC => AudioCodec::Flac,
        Id::OPUS => AudioCodec::Opus,
        Id::VORBIS => AudioCodec::Vorbis,
        Id::PCM_S16LE => AudioCodec::Pcm { bit_depth: 16 },
        Id::PCM_S24LE => AudioCodec::Pcm { bit_depth: 24 },
        Id::PCM_S32LE => AudioCodec::Pcm { bit_depth: 32 },
        other => AudioCodec::Other(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffmpeg::util::color::{Primaries, Space, TransferCharacteristic};

    fn cicp_metadata_from_interpretation(
        interpretation: &DetectedColorInterpretation,
    ) -> Option<VideoColorMetadata> {
        interpretation.evidence.iter().find_map(|evidence| match evidence {
            VideoColorInterpretationEvidence::ExactCicpTags {
                primaries, transfer, matrix, ..
            }
            | VideoColorInterpretationEvidence::PartialCicpTags {
                primaries,
                transfer,
                matrix,
                ..
            } => Some(VideoColorMetadata {
                primaries: primaries.clone(),
                transfer: transfer.clone(),
                matrix: matrix.clone(),
            }),
            _ => None,
        })
    }

    fn executable_for_yuv(interpretation: &DetectedColorInterpretation) -> Option<ColorSpace> {
        let metadata = cicp_metadata_from_interpretation(interpretation)?;
        interpretation.executable_color_space_from_probe(Some(yuv_sampling()), Some(&metadata), &[])
    }

    fn executable_for_rgb(interpretation: &DetectedColorInterpretation) -> Option<ColorSpace> {
        let metadata = cicp_metadata_from_interpretation(interpretation)?;
        interpretation.executable_color_space_from_probe(Some(rgb_sampling()), Some(&metadata), &[])
    }

    fn yuv_sampling() -> ProvenVideoSampling {
        ProvenVideoSampling {
            pixel_format: PixelFormat::Yuv420p,
            bit_depth: 8,
            has_alpha: false,
        }
    }

    fn rgb_sampling() -> ProvenVideoSampling {
        ProvenVideoSampling {
            pixel_format: PixelFormat::Rgb24,
            bit_depth: 8,
            has_alpha: false,
        }
    }

    #[test]
    fn picture_probe_proves_one_frame_when_png_metadata_omits_count() {
        const ONE_PIXEL_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00,
            0x00, 0xB5, 0x1C, 0x0C, 0x02, 0x00, 0x00, 0x00, 0x0B, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xDA, 0x63, 0x64, 0xF8, 0x0F, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xE3, 0x66,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let root = tempfile::tempdir().expect("picture probe tempdir");
        let path = root.path().join("single.png");
        std::fs::write(&path, ONE_PIXEL_PNG).expect("write PNG fixture");

        let info = probe_media_info(&path).expect("probe PNG");

        assert_eq!(
            info.primary_video().and_then(|video| video.total_frames),
            Some(1)
        );
    }

    #[test]
    fn picture_probe_rejects_a_two_frame_gif() {
        const TWO_FRAME_GIF: &[u8] = &[
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xFF, 0xFF, 0xFF, 0x21, 0xF9, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2C,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00,
            0x21, 0xF9, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x01,
            0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x4C, 0x01, 0x00, 0x3B,
        ];
        let root = tempfile::tempdir().expect("picture probe tempdir");
        let path = root.path().join("animated.gif");
        std::fs::write(&path, TWO_FRAME_GIF).expect("write GIF fixture");

        let exactly_one = probe_exactly_one_video_frame(&path, 0).expect("probe GIF frames");

        assert!(!exactly_one);
    }

    #[test]
    fn probe_mapping_keeps_unknown_frame_rate_and_pixel_format_unproven() {
        assert_eq!(
            map_rational(ffmpeg::Rational(0, 0)),
            (Rational::new(0, 1), false)
        );
        assert_eq!(
            map_pixel_format(ffmpeg::util::format::pixel::Pixel::None),
            None
        );
    }

    #[test]
    fn stream_duration_uses_stream_time_base_and_fails_closed() {
        assert_eq!(
            duration_from_stream_ticks(86_400_000, ffmpeg::Rational(1, 48_000)),
            Some(Duration::from_secs(30 * 60))
        );
        assert_eq!(
            duration_from_stream_ticks(0, ffmpeg::Rational(1, 48_000)),
            None
        );
        assert_eq!(duration_from_stream_ticks(1, ffmpeg::Rational(0, 1)), None);
    }

    #[test]
    fn probe_preserves_primary_video_stream_duration() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/small/h264-bframes.mp4");
        let info = probe_media_info(&path).expect("probe checked-in video fixture");
        let duration = info
            .primary_video()
            .and_then(|video| video.duration)
            .expect("fixture video stream duration");

        assert_eq!(duration, Duration::from_millis(1_200));
    }

    #[test]
    fn probe_mapping_canonicalizes_container_quantization_near_nominal_rate() {
        assert_eq!(
            map_rational(ffmpeg::Rational(19_200_000, 800_791)),
            (Rational::FPS_23976, true)
        );
        assert_eq!(
            map_rational(ffmpeg::Rational(24_01, 100)),
            (Rational::new(24_01, 100), true),
            "a rate outside the quantization tolerance must remain exact"
        );
    }

    #[test]
    fn probe_mapping_preserves_exact_h264_high_and_hevc_main10_profiles() {
        assert_eq!(
            map_video_codec_profile(ffmpeg::codec::Profile::H264(
                ffmpeg::codec::profile::H264::High
            )),
            VideoCodecProfile::H264High
        );
        assert_eq!(
            map_video_codec_profile(ffmpeg::codec::Profile::HEVC(
                ffmpeg::codec::profile::HEVC::Main10
            )),
            VideoCodecProfile::HevcMain10
        );
        assert_eq!(
            map_video_codec_profile(ffmpeg::codec::Profile::Unknown),
            VideoCodecProfile::Unknown
        );
    }

    #[test]
    fn detect_color_space_marks_hdr_transfer_metadata() {
        let pq = detect_color_space(
            Primaries::BT2020,
            TransferCharacteristic::SMPTE2084,
            Space::BT2020NCL,
        );
        assert_eq!(pq.candidate_color_space, Some(ColorSpace::Rec2100Pq));
        assert_eq!(executable_for_yuv(&pq), Some(ColorSpace::Rec2100Pq));
        assert_eq!(pq.confidence, VideoColorInterpretationConfidence::High);
        assert_eq!(pq.source, VideoColorSpaceSource::Metadata);
        assert_eq!(pq.method, VideoColorDetectionMethod::CicpTags);
        assert!(matches!(
            pq.evidence.first(),
            Some(VideoColorInterpretationEvidence::ExactCicpTags {
                detected_color_space: ColorSpace::Rec2100Pq,
                ..
            })
        ));

        let hlg = detect_color_space(
            Primaries::BT2020,
            TransferCharacteristic::ARIB_STD_B67,
            Space::BT2020NCL,
        );
        assert_eq!(hlg.candidate_color_space, Some(ColorSpace::Rec2100Hlg));
        assert_eq!(executable_for_yuv(&hlg), Some(ColorSpace::Rec2100Hlg));
        assert_eq!(hlg.confidence, VideoColorInterpretationConfidence::High);
        assert_eq!(hlg.source, VideoColorSpaceSource::Metadata);
        assert_eq!(hlg.method, VideoColorDetectionMethod::CicpTags);
    }

    #[test]
    fn unsupported_cicp_combination_remains_unknown_with_raw_evidence() {
        let metadata = capture_color_metadata(
            Primaries::SMPTE432,
            TransferCharacteristic::SMPTE2084,
            Space::RGB,
        );
        let detection =
            detect_color_space_from_metadata(&metadata, &[], None, Some(rgb_sampling()));

        assert_eq!(detection.candidate_color_space, None);
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::None
        );
        assert_eq!(detection.source, VideoColorSpaceSource::UnsupportedMetadata);
        assert_eq!(
            detection.method,
            VideoColorDetectionMethod::UnsupportedCicpTags
        );
        assert!(matches!(
            detection.evidence.first(),
            Some(VideoColorInterpretationEvidence::UnsupportedCicpTags {
                primaries,
                transfer,
                matrix,
            }) if primaries.name.as_deref() == Some("smpte432")
                && transfer.name.as_deref() == Some("smpte2084")
                && matrix.name.as_deref().is_some_and(|name| name == "gbr" || name == "rgb")
        ));
        assert!(detection
            .warnings
            .contains(&VideoColorInterpretationWarning::UnsupportedCicpTags));
        assert!(!detection.warnings.iter().any(|warning| matches!(
            warning,
            VideoColorInterpretationWarning::PartialCicpTags { .. }
        )));

        let diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Full,
            sampling: None,
            interpretation: detection,
            metadata: Some(metadata),
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        };
        let summary = diagnostic.issue_summary();
        assert_eq!(summary.missing_cicp_tags, 0);
        assert_eq!(summary.unsupported_cicp_tags, 1);

        let mut aggregate = VideoColorDiagnosticIssueAggregate::default();
        aggregate.observe(&diagnostic);
        assert_eq!(aggregate.method_missing_metadata, 0);
        assert_eq!(aggregate.method_unsupported_cicp_tags, 1);
        assert_eq!(aggregate.missing_cicp_tags, 0);
        assert_eq!(aggregate.unsupported_cicp_tags, 1);
    }

    #[test]
    fn declared_camera_metadata_preserves_unsupported_cicp_as_a_warning() {
        let metadata = capture_color_metadata(
            Primaries::SMPTE432,
            TransferCharacteristic::SMPTE2084,
            Space::RGB,
        );
        let hint = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "camera_profile",
            "ARRI LogC4 / AWG4",
        )
        .expect("declared camera profile");

        let detection = interpret_video_color_metadata(
            &metadata,
            Some(rgb_sampling()),
            std::slice::from_ref(&hint),
        );

        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::ArriLogC4WideGamut4)
        );
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::MetadataHint);
        assert!(detection
            .warnings
            .contains(&VideoColorInterpretationWarning::UnsupportedCicpTags));
        assert!(detection.evidence.iter().any(|evidence| matches!(
            evidence,
            VideoColorInterpretationEvidence::UnsupportedCicpTags { .. }
        )));
    }

    #[test]
    fn detect_color_space_does_not_invent_srgb_from_rgb_sampling() {
        let detection = detect_color_space(
            Primaries::BT709,
            TransferCharacteristic::Unspecified,
            Space::RGB,
        );

        assert_eq!(detection.candidate_color_space, None);
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::None
        );
        assert_eq!(detection.source, VideoColorSpaceSource::UnsupportedMetadata);
        assert_eq!(
            detection.method,
            VideoColorDetectionMethod::UnsupportedCicpTags
        );
        assert!(detection
            .warnings
            .contains(&VideoColorInterpretationWarning::UnsupportedCicpTags));
    }

    #[test]
    fn detect_color_space_uses_rgb_sampling_without_losing_rec709_colorimetry() {
        let detection =
            detect_color_space(Primaries::BT709, TransferCharacteristic::BT709, Space::RGB);

        assert_eq!(detection.candidate_color_space, Some(ColorSpace::Rec709));
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
        assert_eq!(
            executable_for_rgb(&detection),
            Some(ColorSpace::Rec709),
            "complete CICP plus proven RGB sampling is executable without a YCbCr matrix"
        );
    }

    #[test]
    fn detect_color_space_recognizes_exact_pal_and_ntsc_rec601_tags() {
        let pal = detect_color_space(
            Primaries::BT470BG,
            TransferCharacteristic::GAMMA28,
            Space::BT470BG,
        );
        assert_eq!(pal.candidate_color_space, Some(ColorSpace::Rec601Pal));
        assert_eq!(pal.confidence, VideoColorInterpretationConfidence::High);

        let ntsc = detect_color_space(
            Primaries::SMPTE170M,
            TransferCharacteristic::SMPTE170M,
            Space::SMPTE170M,
        );
        assert_eq!(ntsc.candidate_color_space, Some(ColorSpace::Rec601Ntsc));
        assert_eq!(ntsc.confidence, VideoColorInterpretationConfidence::High);
    }

    #[test]
    fn detect_color_space_uses_matrix_metadata_when_primaries_are_missing() {
        let detection = detect_color_space(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::BT709,
        );

        assert_eq!(detection.candidate_color_space, Some(ColorSpace::Rec709));
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Medium
        );
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
        assert!(matches!(
            detection.evidence.first(),
            Some(VideoColorInterpretationEvidence::PartialCicpTags {
                detected_color_space: ColorSpace::Rec709,
                ..
            })
        ));
        assert_eq!(executable_for_rgb(&detection), None);

        let mut unproven = detection.clone();
        unproven.evidence.clear();
        assert_eq!(
            executable_for_rgb(&unproven),
            None,
            "partial CICP remains non-executable regardless of confidence"
        );
        let mut contradictory = detection;
        contradictory.candidate_color_space = Some(ColorSpace::Rec2020);
        assert_eq!(executable_for_rgb(&contradictory), None);
    }

    #[test]
    fn complete_colorimetry_requires_explicit_supported_matrix_for_yuv() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::Unspecified,
        );
        let detection = detect_color_space_from_metadata(&metadata, &[], None, None);

        assert_eq!(detection.candidate_color_space, Some(ColorSpace::Rec709));
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(ProvenVideoSampling {
                    pixel_format: PixelFormat::Yuv420p,
                    bit_depth: 8,
                    has_alpha: false,
                }),
                Some(&metadata),
                &[],
            ),
            None
        );

        let rgb_sampling = ProvenVideoSampling {
            pixel_format: PixelFormat::Rgb24,
            bit_depth: 8,
            has_alpha: false,
        };
        let rgb = detect_color_space_from_metadata(&metadata, &[], None, Some(rgb_sampling));
        assert_eq!(
            rgb.executable_color_space_from_probe(Some(rgb_sampling), Some(&metadata), &[]),
            Some(ColorSpace::Rec709),
            "proven RGB sampling has no YCbCr matrix obligation"
        );
    }

    #[test]
    fn explicit_supported_yuv_matrix_is_independent_of_rgb_colorimetry() {
        let detection = detect_color_space(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT2020NCL,
        );

        assert_eq!(detection.candidate_color_space, Some(ColorSpace::Rec709));
        assert_eq!(executable_for_yuv(&detection), Some(ColorSpace::Rec709));
    }

    #[test]
    fn detect_color_space_does_not_claim_missing_metadata_as_rec709() {
        let detection = detect_color_space(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );

        assert_eq!(detection.candidate_color_space, None);
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::None
        );
        assert_eq!(detection.source, VideoColorSpaceSource::MissingMetadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::MissingMetadata);
        assert!(detection.warnings.contains(&VideoColorInterpretationWarning::MissingCicpTags));
    }

    #[test]
    fn detect_color_space_uses_icc_profile_when_cicp_is_missing() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let icc_profile = IccColorProfileHint {
            mapping: mondrian_core::icc::IccColorSpaceMapping::Mapped {
                color_space: ColorSpace::DisplayP3,
                method: mondrian_core::icc::IccColorSpaceMappingMethod::ProfileName,
            },
            profile_name: Some("Display P3".to_owned()),
        };

        let detection = detect_color_space_from_metadata(
            &metadata,
            &[],
            Some(&icc_profile),
            Some(rgb_sampling()),
        );

        assert_eq!(detection.candidate_color_space, Some(ColorSpace::DisplayP3));
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Medium
        );
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::IccProfile);
        assert_eq!(
            detection
                .executable_color_space_from_probe(Some(rgb_sampling()), Some(&metadata), &[],),
            None,
            "a profile description is not verified chromaticity/TRC evidence"
        );
        assert!(
            detection.evidence.contains(&VideoColorInterpretationEvidence::IccProfile {
                mapped_color_space: Some(ColorSpace::DisplayP3),
                profile_name: Some("Display P3".to_owned()),
            })
        );
        assert!(detection.warnings.contains(&VideoColorInterpretationWarning::MissingCicpTags));
    }

    #[test]
    fn icc_profile_keeps_conflicting_descriptive_hint_as_ignored_evidence() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let hint = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Container,
            key: "comment".to_string(),
            value: "converted from Apple Log".to_string(),
            detected_color_space: ColorSpace::AppleLogBt2020,
            authority: VideoColorMetadataHintAuthority::DiagnosticSuggestion,
        };
        let icc_profile = IccColorProfileHint {
            mapping: mondrian_core::icc::IccColorSpaceMapping::Mapped {
                color_space: ColorSpace::DisplayP3,
                method: mondrian_core::icc::IccColorSpaceMappingMethod::ProfileName,
            },
            profile_name: Some("Display P3".to_owned()),
        };

        let detection = detect_color_space_from_metadata(
            &metadata,
            std::slice::from_ref(&hint),
            Some(&icc_profile),
            Some(rgb_sampling()),
        );

        assert_eq!(detection.candidate_color_space, Some(ColorSpace::DisplayP3));
        assert_eq!(detection.method, VideoColorDetectionMethod::IccProfile);
        assert!(detection.evidence.contains(&metadata_hint_evidence(&hint)));
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::LowerPriorityMetadataHints {
                selected_method: VideoColorDetectionMethod::IccProfile,
                selected_color_space: ColorSpace::DisplayP3,
                ignored: vec![hint],
            }
        ));
    }

    #[test]
    fn unmapped_icc_profile_never_becomes_implicit_rec709() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let icc_profile = IccColorProfileHint {
            mapping: mondrian_core::icc::IccColorSpaceMapping::Unmapped {
                profile_color_space: "RGB".to_owned(),
                reason: "generic RGB profile has no managed mapping".to_owned(),
            },
            profile_name: Some("Generic Monitor Profile".to_owned()),
        };

        let detection = detect_color_space_from_metadata(
            &metadata,
            &[],
            Some(&icc_profile),
            Some(rgb_sampling()),
        );

        assert_eq!(detection.candidate_color_space, None);
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::None
        );
        assert_eq!(detection.method, VideoColorDetectionMethod::MissingMetadata);
        assert!(
            detection.evidence.contains(&VideoColorInterpretationEvidence::IccProfile {
                mapped_color_space: None,
                profile_name: Some("Generic Monitor Profile".to_owned()),
            })
        );
        assert!(detection.warnings.iter().any(|warning| matches!(
            warning,
            VideoColorInterpretationWarning::IccProfileUnmapped { profile_name, reason }
                if profile_name.as_deref() == Some("Generic Monitor Profile")
                    && reason.contains("no managed mapping")
        )));

        let diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: detection,
            metadata: Some(metadata),
            metadata_hints: Vec::new(),
            hdr_metadata: vec![VideoHdrMetadataSummary {
                kind: VideoHdrSideDataKind::IccProfile,
                payload_size: 128,
                payload: None,
            }],
        };
        let summary = diagnostic.issue_summary();
        assert_eq!(summary.icc_profile_unmapped, 1);
        assert!(summary.has_icc_profile);
        assert!(summary.has_user_visible_warnings);
    }

    #[test]
    fn unmapped_icc_does_not_hide_complete_low_confidence_metadata_hint() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let hint = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::FileName,
            key: "filename".to_owned(),
            value: "A001_Sony_S-Log3_S-Gamut3.Cine.mov".to_owned(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            authority: VideoColorMetadataHintAuthority::DiagnosticSuggestion,
        };
        let icc_profile = IccColorProfileHint {
            mapping: mondrian_core::icc::IccColorSpaceMapping::Unmapped {
                profile_color_space: "RGB".to_owned(),
                reason: "unknown primaries".to_owned(),
            },
            profile_name: Some("Unknown RGB".to_owned()),
        };

        let detection = detect_color_space_from_metadata(
            &metadata,
            std::slice::from_ref(&hint),
            Some(&icc_profile),
            Some(rgb_sampling()),
        );

        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Low
        );
        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(rgb_sampling()),
                Some(&metadata),
                std::slice::from_ref(&hint),
            ),
            None
        );
        assert!(detection.evidence.contains(&icc_profile_evidence(&icc_profile)));
        assert!(detection.warnings.iter().any(|warning| matches!(
            warning,
            VideoColorInterpretationWarning::IccProfileUnmapped { .. }
        )));
    }

    #[test]
    fn detect_color_space_reports_icc_cicp_mismatch_without_changing_cicp_decision() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let icc_profile = IccColorProfileHint {
            mapping: mondrian_core::icc::IccColorSpaceMapping::Mapped {
                color_space: ColorSpace::DisplayP3,
                method: mondrian_core::icc::IccColorSpaceMappingMethod::ProfileName,
            },
            profile_name: Some("Display P3".to_owned()),
        };

        let detection = detect_color_space_from_metadata(
            &metadata,
            &[],
            Some(&icc_profile),
            Some(yuv_sampling()),
        );

        assert_eq!(detection.candidate_color_space, Some(ColorSpace::Rec709));
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::High
        );
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
        assert!(
            detection.evidence.contains(&VideoColorInterpretationEvidence::IccProfile {
                mapped_color_space: Some(ColorSpace::DisplayP3),
                profile_name: Some("Display P3".to_owned()),
            })
        );
        assert!(
            detection.warnings.contains(&VideoColorInterpretationWarning::IccCicpMismatch {
                icc_color_space: ColorSpace::DisplayP3,
                cicp_color_space: ColorSpace::Rec709,
            })
        );
    }

    #[test]
    fn metadata_hint_identifies_camera_log_spaces() {
        let slog3 = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.sony.colorProfile",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("slog3 metadata hint");
        assert_eq!(slog3.detected_color_space, ColorSpace::SonySLog3SGamut3Cine);
        assert!(slog3.is_executable_declaration());

        let apple_log = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Container,
            "com.apple.proapps.cameraLog",
            "Apple Log",
        )
        .expect("apple log metadata hint");
        assert_eq!(apple_log.detected_color_space, ColorSpace::AppleLogBt2020);
        assert!(apple_log.is_executable_declaration());

        let logc4 = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "camera_profile",
            "ARRI LogC4",
        )
        .expect("arri logc4 metadata hint");
        assert_eq!(logc4.detected_color_space, ColorSpace::ArriLogC4WideGamut4);
        assert_eq!(
            logc4.authority,
            VideoColorMetadataHintAuthority::DiagnosticSuggestion,
            "a generic dictionary key has no acquisition-source semantics"
        );
    }

    #[test]
    fn arbitrary_key_with_declaration_suffix_stays_diagnostic_only() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let hint = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "not_a_color_space",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("recognized value remains visible as a diagnostic candidate");

        assert_eq!(
            hint.authority,
            VideoColorMetadataHintAuthority::DiagnosticSuggestion
        );
        let mut detection = interpret_video_color_metadata(
            &metadata,
            Some(rgb_sampling()),
            std::slice::from_ref(&hint),
        );
        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(rgb_sampling()),
                Some(&metadata),
                std::slice::from_ref(&hint),
            ),
            None
        );
        detection.confidence = VideoColorInterpretationConfidence::High;
        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(rgb_sampling()),
                Some(&metadata),
                std::slice::from_ref(&hint),
            ),
            None,
            "confidence cannot upgrade a free-form field into execution authority"
        );
    }

    #[test]
    fn namespaced_camera_profile_is_an_explicit_high_confidence_declaration() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let hint = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.sony.colorProfile",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("complete Sony color profile declaration");

        let detection = interpret_video_color_metadata(
            &metadata,
            Some(yuv_sampling()),
            std::slice::from_ref(&hint),
        );

        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::High
        );
        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(yuv_sampling()),
                Some(&metadata),
                std::slice::from_ref(&hint),
            ),
            Some(ColorSpace::SonySLog3SGamut3Cine),
            "an exact namespaced declaration may override contradictory CICP color identity once the raw sampling and matrix are proven"
        );
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::MetadataHintOverridesCicpTags {
                selected: hint,
                cicp_color_space: ColorSpace::Rec709,
                cicp_metadata: metadata,
            }
        ));
    }

    #[test]
    fn metadata_hint_identifies_scene_linear_aces_and_slog2_sources() {
        for (key, value, expected) in [
            ("oiio:ColorSpace", "ACES2065-1", ColorSpace::Aces2065_1),
            ("ocio:ColorSpace", "ACEScg", ColorSpace::AcesCg),
            ("ocio:ColorSpace", "ACEScct", ColorSpace::AcesCct),
            (
                "ocio:ColorSpace",
                "Linear Rec.2020",
                ColorSpace::LinearRec2020,
            ),
            (
                "camera_profile",
                "Sony S-Log2 / S-Gamut",
                ColorSpace::SonySLog2SGamut,
            ),
        ] {
            let hint =
                parse_video_color_metadata_hint(VideoColorMetadataHintScope::Stream, key, value)
                    .unwrap_or_else(|| panic!("missing metadata hint for {key}={value}"));
            assert_eq!(hint.detected_color_space, expected);
        }
    }

    #[test]
    fn declared_metadata_parser_covers_every_supported_camera_log_gamut_pair() {
        let cases = [
            ("Apple Log", ColorSpace::AppleLogBt2020),
            ("Sony S-Log2 / S-Gamut", ColorSpace::SonySLog2SGamut),
            ("Sony S-Log3 / S-Gamut3", ColorSpace::SonySLog3SGamut3),
            (
                "Sony S-Log3 / S-Gamut3.Cine",
                ColorSpace::SonySLog3SGamut3Cine,
            ),
            ("ARRI LogC3 / AWG3", ColorSpace::ArriLogC3WideGamut3),
            ("ARRI LogC4 / AWG4", ColorSpace::ArriLogC4WideGamut4),
            (
                "Canon Log2 / Cinema Gamut D55",
                ColorSpace::CanonLog2CinemaGamutD55,
            ),
            (
                "Canon Log3 / Cinema Gamut D55",
                ColorSpace::CanonLog3CinemaGamutD55,
            ),
            ("Panasonic V-Log / V-Gamut", ColorSpace::PanasonicVLogVGamut),
            (
                "RED Log3G10 / REDWideGamutRGB",
                ColorSpace::RedLog3G10WideGamutRgb,
            ),
            (
                "Blackmagic Film / Wide Gamut Gen 5",
                ColorSpace::BlackmagicFilmWideGamutGen5,
            ),
            ("DJI D-Log / D-Gamut", ColorSpace::DjiDLogDGamut),
            (
                "DaVinci Intermediate / Wide Gamut",
                ColorSpace::DavinciIntermediateWideGamut,
            ),
        ];

        for (value, expected) in cases {
            let hint = parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Stream,
                "camera_profile",
                value,
            )
            .unwrap_or_else(|| panic!("supported metadata declaration was not parsed: {value}"));
            assert_eq!(hint.detected_color_space, expected, "value={value}");
        }
    }

    #[test]
    fn complete_filename_pair_is_only_a_low_confidence_fallback() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let hint = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::FileName,
            "filename",
            "A001_Sony_S-Log3_S-Gamut3.Cine.mov",
        )
        .expect("complete filename pair should be preserved as evidence");

        let detection = interpret_video_color_metadata(
            &metadata,
            Some(yuv_sampling()),
            std::slice::from_ref(&hint),
        );

        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Low
        );
        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(rgb_sampling()),
                Some(&metadata),
                std::slice::from_ref(&hint),
            ),
            None
        );
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::DescriptiveMetadataHintInference { selected: hint }
        ));
    }

    #[test]
    fn metadata_hint_ignores_ambiguous_log_words() {
        assert_eq!(
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Container,
                "log",
                "enabled"
            ),
            None
        );
        assert_eq!(
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Stream,
                "camera_profile",
                "S-Log3",
            ),
            None
        );
        assert_eq!(
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::FileName,
                "filename",
                "clip-Sony-Slog3.mov",
            ),
            None
        );
        assert_eq!(
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::FileName,
                "filename",
                "clip-RED-Log3G10.mov",
            ),
            None
        );
    }

    #[test]
    fn exact_vendor_declaration_overrides_cicp_delivery_tags_for_camera_log() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let hint = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.sony.colorProfile",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("Sony declaration");

        let detection = detect_color_space_from_metadata(
            &metadata,
            std::slice::from_ref(&hint),
            None,
            Some(yuv_sampling()),
        );

        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::High
        );
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::MetadataHint);
        assert!(matches!(
            detection.evidence.first(),
            Some(VideoColorInterpretationEvidence::MetadataHint {
                detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
                ..
            })
        ));
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::MetadataHintOverridesCicpTags {
                selected: hint,
                cicp_color_space: ColorSpace::Rec709,
                cicp_metadata: metadata
            }
        ));
    }

    #[test]
    fn descriptive_metadata_text_cannot_override_exact_cicp_tags() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let hint = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Container,
            key: "comment".to_string(),
            value: "graded from S-Log3 / S-Gamut3.Cine".to_string(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            authority: VideoColorMetadataHintAuthority::DiagnosticSuggestion,
        };

        let detection = interpret_video_color_metadata(
            &metadata,
            Some(yuv_sampling()),
            std::slice::from_ref(&hint),
        );

        assert_eq!(detection.candidate_color_space, Some(ColorSpace::Rec709));
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::High
        );
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
        assert!(detection.evidence.contains(&metadata_hint_evidence(&hint)));
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::LowerPriorityMetadataHints {
                selected_method: VideoColorDetectionMethod::CicpTags,
                selected_color_space: ColorSpace::Rec709,
                ignored: vec![hint.clone()],
            }
        ));
        let diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: detection.clone(),
            metadata: Some(metadata),
            metadata_hints: vec![hint],
            hdr_metadata: Vec::new(),
        };
        let summary = diagnostic.issue_summary();
        assert_eq!(summary.lower_priority_metadata_hints, 1);
        assert_eq!(summary.ignored_lower_priority_metadata_hints, 1);
    }

    #[test]
    fn descriptive_metadata_text_is_a_diagnosed_low_confidence_fallback() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let hint = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Container,
            key: "comment".to_string(),
            value: "source is S-Log3 / S-Gamut3.Cine".to_string(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            authority: VideoColorMetadataHintAuthority::DiagnosticSuggestion,
        };

        let mut detection = interpret_video_color_metadata(
            &metadata,
            Some(rgb_sampling()),
            std::slice::from_ref(&hint),
        );

        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Low
        );
        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(rgb_sampling()),
                Some(&metadata),
                std::slice::from_ref(&hint),
            ),
            None
        );
        detection.confidence = VideoColorInterpretationConfidence::High;
        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(rgb_sampling()),
                Some(&metadata),
                std::slice::from_ref(&hint),
            ),
            None,
            "descriptive provenance must remain non-executable if confidence scoring changes"
        );
        assert_eq!(detection.method, VideoColorDetectionMethod::MetadataHint);
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::DescriptiveMetadataHintInference { selected: hint }
        ));
    }

    #[test]
    fn explicit_metadata_declaration_wins_independent_of_dictionary_order() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let descriptive = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "comment".to_string(),
            value: "converted from S-Log3 / S-Gamut3.Cine".to_string(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            authority: VideoColorMetadataHintAuthority::DiagnosticSuggestion,
        };
        let declared = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.sony.colorProfile",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("Sony declaration");

        let forward = interpret_video_color_metadata(
            &metadata,
            Some(rgb_sampling()),
            &[descriptive.clone(), declared.clone()],
        );
        let reverse = interpret_video_color_metadata(
            &metadata,
            Some(rgb_sampling()),
            &[declared.clone(), descriptive.clone()],
        );

        assert_eq!(
            forward, reverse,
            "probe interpretation must not depend on metadata dictionary order"
        );
        assert_eq!(
            forward.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(forward.confidence, VideoColorInterpretationConfidence::High);
        assert_eq!(
            forward.executable_color_space_from_probe(
                Some(rgb_sampling()),
                Some(&metadata),
                &[descriptive.clone(), declared],
            ),
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert!(
            !forward.warnings.iter().any(|warning| matches!(
                warning,
                VideoColorInterpretationWarning::MultipleMetadataHints { .. }
            )),
            "a corroborating diagnostic comment is evidence, not an ambiguity"
        );
    }

    #[test]
    fn stream_declaration_has_stable_priority_over_container_declaration() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let container = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Container,
            key: "camera_profile".to_string(),
            value: "Apple Log".to_string(),
            detected_color_space: ColorSpace::AppleLogBt2020,
            authority: VideoColorMetadataHintAuthority::SourceDeclaration(
                VideoColorMetadataDeclaration::SourceColorSpace,
            ),
        };
        let stream = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "camera_profile".to_string(),
            value: "ARRI LogC4".to_string(),
            detected_color_space: ColorSpace::ArriLogC4WideGamut4,
            authority: VideoColorMetadataHintAuthority::SourceDeclaration(
                VideoColorMetadataDeclaration::SourceColorSpace,
            ),
        };

        let detection = interpret_video_color_metadata(
            &metadata,
            Some(yuv_sampling()),
            &[container.clone(), stream.clone()],
        );

        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::ArriLogC4WideGamut4)
        );
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::MultipleMetadataHints {
                selected: stream,
                ignored: vec![container],
            }
        ));
    }

    #[test]
    fn conflicting_stream_declarations_are_diagnostic_only_in_any_probe_order() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let sony = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.sony.colorProfile",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("Sony declaration");
        let apple = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.apple.proapps.cameraLog",
            "Apple Log",
        )
        .expect("Apple declaration");

        let forward_hints = vec![sony.clone(), apple.clone()];
        let reverse_hints = vec![apple.clone(), sony.clone()];
        let forward =
            interpret_video_color_metadata(&metadata, Some(yuv_sampling()), &forward_hints);
        let reverse =
            interpret_video_color_metadata(&metadata, Some(yuv_sampling()), &reverse_hints);
        assert_eq!(
            forward.candidate_color_space, reverse.candidate_color_space,
            "even the diagnostic representative must not depend on dictionary order"
        );

        for (hints, detection) in [(forward_hints, forward), (reverse_hints, reverse)] {
            assert_eq!(
                detection.confidence,
                VideoColorInterpretationConfidence::Medium
            );
            assert_eq!(
                detection.executable_color_space_from_probe(
                    Some(yuv_sampling()),
                    Some(&metadata),
                    &hints,
                ),
                None,
                "probe dictionary order cannot select a pixel identity"
            );
            assert!(detection.evidence.contains(&metadata_hint_evidence(&sony)));
            assert!(detection.evidence.contains(&metadata_hint_evidence(&apple)));
            assert!(detection.warnings.iter().any(|warning| matches!(
                warning,
                VideoColorInterpretationWarning::MultipleMetadataHints { .. }
            )));
        }
    }

    #[test]
    fn agreeing_stream_declarations_merge_and_override_container_scope() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let sony = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.sony.colorProfile",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("Sony declaration");
        let apple = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Container,
            "com.apple.proapps.cameraLog",
            "Apple Log",
        )
        .expect("Apple declaration");
        let hints = vec![apple, sony.clone(), sony];

        let detection = interpret_video_color_metadata(&metadata, Some(yuv_sampling()), &hints);

        assert_eq!(
            detection.executable_color_space_from_probe(
                Some(yuv_sampling()),
                Some(&metadata),
                &hints,
            ),
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
    }

    #[test]
    fn multiple_metadata_hints_keep_selected_and_ignored_evidence() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let hints = vec![
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Stream,
                "com.sony.colorProfile",
                "S-Log3 / S-Gamut3.Cine",
            )
            .expect("Sony declaration"),
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Container,
                "com.apple.proapps.cameraLog",
                "Apple Log",
            )
            .expect("Apple declaration"),
        ];

        let detection =
            detect_color_space_from_metadata(&metadata, &hints, None, Some(yuv_sampling()));

        assert_eq!(
            detection.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(detection.evidence.len(), 2);
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::MultipleMetadataHints {
                selected: hints[0].clone(),
                ignored: vec![hints[1].clone()],
            }
        ));
    }

    #[test]
    fn video_color_diagnostic_summary_includes_warning_context() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let hint = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.sony.colorProfile",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("Sony declaration");
        let interpretation = detect_color_space_from_metadata(
            &metadata,
            std::slice::from_ref(&hint),
            None,
            Some(yuv_sampling()),
        );
        let diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Limited,
            sampling: Some(yuv_sampling()),
            interpretation,
            metadata: Some(metadata),
            metadata_hints: vec![hint],
            hdr_metadata: Vec::new(),
        };

        let summary = diagnostic.summary();

        assert!(summary.contains("hint_overrides_cicp"));
        assert!(summary.contains(
            "Stream:SourceDeclaration(SonyColorProfile):com.sony.colorProfile=S-Log3 / S-Gamut3.Cine->SonySLog3SGamut3Cine"
        ));
        assert!(summary.contains("cicp=Rec709"));
        assert!(summary.contains("primaries=bt709"));
    }

    #[test]
    fn video_color_diagnostic_issue_summary_counts_structured_warnings_and_hdr() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let hints = vec![
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Stream,
                "com.sony.colorProfile",
                "S-Log3 / S-Gamut3.Cine",
            )
            .expect("Sony declaration"),
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Container,
                "com.apple.proapps.cameraLog",
                "Apple Log",
            )
            .expect("Apple declaration"),
        ];
        let interpretation =
            detect_color_space_from_metadata(&metadata, &hints, None, Some(yuv_sampling()));
        let diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Limited,
            sampling: Some(yuv_sampling()),
            interpretation,
            metadata: Some(metadata),
            metadata_hints: hints,
            hdr_metadata: vec![
                VideoHdrMetadataSummary {
                    kind: VideoHdrSideDataKind::MasteringDisplayMetadata,
                    payload_size: 88,
                    payload: None,
                },
                VideoHdrMetadataSummary {
                    kind: VideoHdrSideDataKind::ContentLightLevel,
                    payload_size: 8,
                    payload: None,
                },
                VideoHdrMetadataSummary {
                    kind: VideoHdrSideDataKind::IccProfile,
                    payload_size: 128,
                    payload: None,
                },
            ],
        };

        assert_eq!(
            diagnostic.issue_summary(),
            VideoColorDiagnosticIssueSummary {
                executable_color_space: Some(ColorSpace::SonySLog3SGamut3Cine),
                confidence: VideoColorInterpretationConfidence::High,
                source: VideoColorSpaceSource::Metadata,
                method: VideoColorDetectionMethod::MetadataHint,
                has_raw_cicp_metadata: true,
                metadata_hint_count: 2,
                evidence_count: 2,
                warning_count: 2,
                multiple_metadata_hints: 1,
                ignored_metadata_hints: 1,
                metadata_hint_overrides_cicp_tags: 1,
                lower_priority_metadata_hints: 0,
                ignored_lower_priority_metadata_hints: 0,
                partial_cicp_tags: 0,
                missing_cicp_tags: 0,
                unsupported_cicp_tags: 0,
                decoder_unavailable: 0,
                hdr_side_data_count: 3,
                has_mastering_display_metadata: true,
                has_content_light_metadata: true,
                has_dynamic_hdr10_plus: false,
                has_dolby_vision_config: false,
                has_icc_profile: true,
                icc_cicp_mismatch: 0,
                icc_profile_unmapped: 0,
                has_user_visible_warnings: true,
            }
        );
    }

    #[test]
    fn video_color_diagnostic_issue_summary_reports_missing_and_decoder_unavailable() {
        let missing_interpretation = detect_color_space(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let missing_diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: missing_interpretation,
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        };
        let missing_summary = missing_diagnostic.issue_summary();

        assert_eq!(missing_summary.executable_color_space, None);
        assert_eq!(
            missing_summary.confidence,
            VideoColorInterpretationConfidence::None
        );
        assert!(!missing_summary.has_raw_cicp_metadata);
        assert_eq!(missing_summary.missing_cicp_tags, 1);
        assert_eq!(missing_summary.unsupported_cicp_tags, 0);
        assert_eq!(missing_summary.decoder_unavailable, 0);
        assert!(missing_summary.has_user_visible_warnings);

        let unavailable_interpretation = DetectedColorInterpretation::decoder_unavailable();
        let unavailable_diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: unavailable_interpretation,
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        };
        let unavailable_summary = unavailable_diagnostic.issue_summary();

        assert_eq!(
            unavailable_summary.method,
            VideoColorDetectionMethod::DecoderUnavailable
        );
        assert_eq!(unavailable_summary.decoder_unavailable, 1);
        assert_eq!(unavailable_summary.missing_cicp_tags, 0);
        assert_eq!(unavailable_summary.unsupported_cicp_tags, 0);
        assert!(unavailable_summary.has_user_visible_warnings);
    }

    #[test]
    fn video_color_diagnostic_issue_aggregate_accumulates_machine_readable_totals() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let hints = vec![
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Stream,
                "com.sony.colorProfile",
                "S-Log3 / S-Gamut3.Cine",
            )
            .expect("Sony declaration"),
            parse_video_color_metadata_hint(
                VideoColorMetadataHintScope::Container,
                "com.apple.proapps.cameraLog",
                "Apple Log",
            )
            .expect("Apple declaration"),
        ];
        let metadata_interpretation =
            detect_color_space_from_metadata(&metadata, &hints, None, Some(yuv_sampling()));
        let metadata_diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Limited,
            sampling: Some(yuv_sampling()),
            interpretation: metadata_interpretation,
            metadata: Some(metadata),
            metadata_hints: hints,
            hdr_metadata: vec![
                VideoHdrMetadataSummary {
                    kind: VideoHdrSideDataKind::MasteringDisplayMetadata,
                    payload_size: 88,
                    payload: None,
                },
                VideoHdrMetadataSummary {
                    kind: VideoHdrSideDataKind::ContentLightLevel,
                    payload_size: 8,
                    payload: None,
                },
            ],
        };
        let unavailable_interpretation = DetectedColorInterpretation::decoder_unavailable();
        let unavailable_diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: unavailable_interpretation,
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: vec![VideoHdrMetadataSummary {
                kind: VideoHdrSideDataKind::IccProfile,
                payload_size: 128,
                payload: None,
            }],
        };

        let aggregate = VideoColorDiagnosticIssueAggregate::from_diagnostics([
            &metadata_diagnostic,
            &unavailable_diagnostic,
        ]);

        assert_eq!(
            aggregate,
            VideoColorDiagnosticIssueAggregate {
                diagnostics: 2,
                diagnostics_with_executable_color_space: 1,
                diagnostics_with_warnings: 2,
                diagnostics_with_raw_cicp_metadata: 1,
                diagnostics_with_metadata_hints: 1,
                diagnostics_with_hdr_metadata: 2,
                method_metadata_hint: 1,
                method_icc_profile: 0,
                method_cicp_tags: 0,
                method_missing_metadata: 0,
                method_unsupported_cicp_tags: 0,
                method_decoder_unavailable: 1,
                confidence_high: 1,
                confidence_medium: 0,
                confidence_low: 0,
                confidence_none: 1,
                metadata_hint_count: 2,
                evidence_count: 3,
                warning_count: 3,
                multiple_metadata_hints: 1,
                ignored_metadata_hints: 1,
                metadata_hint_overrides_cicp_tags: 1,
                lower_priority_metadata_hints: 0,
                ignored_lower_priority_metadata_hints: 0,
                partial_cicp_tags: 0,
                missing_cicp_tags: 0,
                unsupported_cicp_tags: 0,
                decoder_unavailable: 1,
                hdr_side_data_count: 3,
                diagnostics_with_mastering_display_metadata: 1,
                diagnostics_with_content_light_metadata: 1,
                diagnostics_with_dynamic_hdr10_plus: 0,
                diagnostics_with_dolby_vision_config: 0,
                diagnostics_with_icc_profile: 1,
                diagnostics_with_icc_cicp_mismatch: 0,
                diagnostics_with_icc_profile_unmapped: 0,
            }
        );
    }

    #[test]
    fn hdr_side_data_kind_mapping_identifies_static_and_dynamic_hdr_metadata() {
        use ffmpeg::codec::packet::side_data::Type;

        assert_eq!(
            map_hdr_side_data_kind(Type::MasteringDisplayMetadata),
            Some(VideoHdrSideDataKind::MasteringDisplayMetadata)
        );
        assert_eq!(
            map_hdr_side_data_kind(Type::ContentLightLevel),
            Some(VideoHdrSideDataKind::ContentLightLevel)
        );
        assert_eq!(
            map_hdr_side_data_kind(Type::DYNAMIC_HDR10_PLUS),
            Some(VideoHdrSideDataKind::DynamicHdr10Plus)
        );
        assert_eq!(map_hdr_side_data_kind(Type::Palette), None);
    }

    #[test]
    fn hdr_mastering_display_payload_parses_and_formats_x265_metadata() {
        use ffmpeg::codec::packet::side_data::Type;

        let raw = FfmpegMasteringDisplayMetadata {
            display_primaries: [
                [raw_q(34_000, 50_000), raw_q(16_000, 50_000)],
                [raw_q(13_250, 50_000), raw_q(34_500, 50_000)],
                [raw_q(7_500, 50_000), raw_q(3_000, 50_000)],
            ],
            white_point: [raw_q(15_635, 50_000), raw_q(16_450, 50_000)],
            min_luminance: raw_q(1, 10_000),
            max_luminance: raw_q(1000, 1),
            has_primaries: 1,
            has_luminance: 1,
        };

        let payload = parse_hdr_metadata_payload(Type::MasteringDisplayMetadata, bytes_of(&raw))
            .expect("valid mastering display payload");

        let VideoHdrMetadataPayload::MasteringDisplay(metadata) = payload else {
            panic!("expected mastering display payload");
        };
        assert_eq!(
            metadata.to_x265_master_display().as_deref(),
            Some("G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)")
        );
    }

    #[test]
    fn hdr_content_light_payload_parses_and_formats_x265_metadata() {
        use ffmpeg::codec::packet::side_data::Type;

        let raw = FfmpegContentLightMetadata { max_cll: 1000, max_fall: 400 };
        let payload = parse_hdr_metadata_payload(Type::ContentLightLevel, bytes_of(&raw))
            .expect("valid content light payload");

        let VideoHdrMetadataPayload::ContentLightLevel(metadata) = payload else {
            panic!("expected content light payload");
        };
        assert_eq!(metadata.to_x265_max_cll(), "1000,400");
    }

    #[test]
    fn undersized_hdr_payload_is_not_parsed() {
        use ffmpeg::codec::packet::side_data::Type;

        assert_eq!(
            parse_hdr_metadata_payload(Type::MasteringDisplayMetadata, &[0; 8]),
            None
        );
        assert_eq!(
            parse_hdr_metadata_payload(Type::ContentLightLevel, &[0; 4]),
            None
        );
    }

    #[test]
    fn hdr_payload_abi_mirrors_ffmpeg_side_data_layout() {
        assert_eq!(std::mem::size_of::<FfmpegRational>(), 8);
        assert_eq!(std::mem::align_of::<FfmpegRational>(), 4);
        assert_eq!(std::mem::size_of::<FfmpegMasteringDisplayMetadata>(), 88);
        assert_eq!(std::mem::align_of::<FfmpegMasteringDisplayMetadata>(), 4);
        assert_eq!(std::mem::size_of::<FfmpegContentLightMetadata>(), 8);
        assert_eq!(std::mem::align_of::<FfmpegContentLightMetadata>(), 4);
    }

    #[test]
    fn capture_color_metadata_preserves_raw_cicp_tags() {
        let metadata = capture_color_metadata(
            Primaries::BT2020,
            TransferCharacteristic::SMPTE2084,
            Space::BT2020NCL,
        );

        assert!(metadata.primaries.specified);
        assert!(metadata.transfer.specified);
        assert!(metadata.matrix.specified);
        assert_eq!(metadata.primaries.name.as_deref(), Some("bt2020"));
        assert_eq!(metadata.transfer.name.as_deref(), Some("smpte2084"));
        assert_eq!(metadata.matrix.name.as_deref(), Some("bt2020nc"));
    }

    #[test]
    fn capture_color_metadata_marks_unspecified_tags() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );

        assert!(!metadata.primaries.specified);
        assert!(!metadata.transfer.specified);
        assert!(!metadata.matrix.specified);
        assert_eq!(metadata.primaries.name, None);
        assert_eq!(metadata.transfer.name, None);
        assert_eq!(metadata.matrix.name, None);
    }

    #[test]
    fn video_color_diagnostic_summary_includes_source_detection_and_raw_tags() {
        let metadata = capture_color_metadata(
            Primaries::BT2020,
            TransferCharacteristic::SMPTE2084,
            Space::BT2020NCL,
        );
        let diagnostic = VideoColorDiagnostic {
            color_range: DecodedVideoRange::Limited,
            sampling: Some(yuv_sampling()),
            interpretation: detect_color_space_from_metadata(
                &metadata,
                &[],
                None,
                Some(yuv_sampling()),
            ),
            metadata: Some(metadata),
            metadata_hints: vec![VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Stream,
                key: "camera_profile".to_string(),
                value: "S-Log3 / S-Gamut3.Cine".to_string(),
                detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
                authority: VideoColorMetadataHintAuthority::SourceDeclaration(
                    VideoColorMetadataDeclaration::SourceColorSpace,
                ),
            }],
            hdr_metadata: vec![VideoHdrMetadataSummary {
                kind: VideoHdrSideDataKind::MasteringDisplayMetadata,
                payload_size: 88,
                payload: Some(VideoHdrMetadataPayload::MasteringDisplay(
                    VideoMasteringDisplayMetadata { primaries: None, luminance: None },
                )),
            }],
        };

        let summary = diagnostic.summary();

        assert!(summary.contains("source=Metadata"));
        assert!(summary.contains("method=CicpTags"));
        assert!(summary.contains("candidate=Rec2100Pq"));
        assert!(summary.contains("executable=Rec2100Pq"));
        assert!(summary.contains("range=Limited"));
        assert!(summary.contains("confidence=High"));
        assert!(summary.contains("overridable=true"));
        assert!(summary.contains("primaries=bt2020"));
        assert!(summary.contains("transfer=smpte2084"));
        assert!(summary.contains("matrix=bt2020nc"));
        assert!(summary.contains("camera_profile=S-Log3 / S-Gamut3.Cine"));
        assert!(summary.contains("MasteringDisplayMetadata(bytes=88,payload=master_display"));
    }

    #[test]
    fn media_probe_reads_static_hdr_metadata_from_first_decoded_frame() {
        let path = std::env::temp_dir().join(format!(
            "mondrian-media-frame-hdr-probe-{}.mp4",
            std::process::id()
        ));
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=16x16:r=1:d=1",
                "-frames:v",
                "1",
                "-c:v",
                "libx265",
                "-pix_fmt",
                "yuv420p10le",
                "-color_range",
                "tv",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "smpte2084",
                "-colorspace",
                "bt2020nc",
                "-x265-params",
                "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400",
            ])
            .arg(&path)
            .output()
            .expect("launch ffmpeg HDR fixture");
        assert!(
            output.status.success(),
            "ffmpeg HDR fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let info = probe_media_info(&path).expect("probe encoded HDR fixture");
        let video = info.primary_video().expect("video stream");
        let mastering = video
            .hdr_metadata
            .iter()
            .find(|summary| summary.kind == VideoHdrSideDataKind::MasteringDisplayMetadata)
            .and_then(|summary| summary.payload.as_ref());
        let content_light = video
            .hdr_metadata
            .iter()
            .find(|summary| summary.kind == VideoHdrSideDataKind::ContentLightLevel)
            .and_then(|summary| summary.payload.as_ref());
        let _ = std::fs::remove_file(path);

        assert!(matches!(
            mastering,
            Some(VideoHdrMetadataPayload::MasteringDisplay(_))
        ));
        assert!(matches!(
            content_light,
            Some(VideoHdrMetadataPayload::ContentLightLevel(_))
        ));
    }

    #[test]
    fn audio_layout_probe_preserves_standard_and_custom_speaker_semantics() {
        assert_eq!(
            map_channel_layout(ffmpeg::ChannelLayout::_5POINT1, 6),
            ChannelLayout::Exact(AudioChannelLayout::Surround51Side)
        );
        assert_eq!(
            map_channel_layout(ffmpeg::ChannelLayout::_5POINT1_BACK, 6),
            ChannelLayout::Exact(AudioChannelLayout::Surround51Back)
        );
        let six_point_zero = AudioChannelLayout::speakers([
            AudioChannelPosition::FrontLeft,
            AudioChannelPosition::FrontRight,
            AudioChannelPosition::FrontCenter,
            AudioChannelPosition::BackCenter,
            AudioChannelPosition::SideLeft,
            AudioChannelPosition::SideRight,
        ])
        .expect("6.0 speaker layout");
        assert_eq!(
            map_channel_layout(ffmpeg::ChannelLayout::_6POINT0, 6),
            ChannelLayout::Exact(six_point_zero)
        );
        assert_eq!(
            ChannelLayout::Exact(AudioChannelLayout::Surround51Side).exact_signal_layout(),
            Some(AudioChannelLayout::Surround51Side)
        );
        assert_eq!(
            ChannelLayout::Exact(AudioChannelLayout::Surround51Back).exact_signal_layout(),
            Some(AudioChannelLayout::Surround51Back)
        );
        assert_eq!(
            ChannelLayout::Exact(AudioChannelLayout::Surround71).exact_signal_layout(),
            Some(AudioChannelLayout::Surround71)
        );
        assert_eq!(
            ChannelLayout::Unspecified(12).exact_signal_layout(),
            Some(mondrian_core::AudioChannelLayout::discrete(12).expect("discrete layout"))
        );
        assert_eq!(ChannelLayout::Unsupported(6).exact_signal_layout(), None);
        assert_eq!(
            exact_signal_layout_from_ffmpeg_mask((1_u64 << 0) | (1_u64 << 63), 2),
            None
        );
        assert_eq!(
            exact_signal_layout_from_ffmpeg_mask(1_u64 << 2, 1),
            Some(AudioChannelLayout::Mono)
        );
    }

    #[test]
    fn pcm_wave_without_a_channel_mask_remains_explicitly_unspecified() {
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().expect("temporary wave");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&36u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8_000u32.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        file.write_all(&bytes).expect("write wave");
        file.flush().expect("flush wave");
        crate::ffmpeg_runtime::ensure_ffmpeg_initialized(file.path()).expect("ffmpeg init");
        let input = ffmpeg::format::input(file.path()).expect("open wave");
        let stream = input.streams().next().expect("audio stream");
        let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .expect("audio context");
        let decoder = context.decoder().audio().expect("audio decoder");

        let layout = decoder.channel_layout();
        assert!(layout.is_empty());
        assert_eq!(
            map_channel_layout(layout, decoder.channels() as u8),
            ChannelLayout::Unspecified(1)
        );
    }

    #[test]
    fn audio_stream_metadata_normalization_does_not_invent_missing_labels() {
        assert_eq!(
            normalized_stream_metadata(Some("  eng ")),
            Some("eng".to_owned())
        );
        assert_eq!(normalized_stream_metadata(Some("  ")), None);
        assert_eq!(normalized_stream_metadata(None), None);
    }

    const fn raw_q(num: i32, den: i32) -> FfmpegRational {
        FfmpegRational { num, den }
    }

    fn bytes_of<T>(value: &T) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(value).cast::<u8>(),
                std::mem::size_of::<T>(),
            )
        }
    }
}
