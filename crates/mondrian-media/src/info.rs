//! 媒体文件元数据探针
//!
//! 使用 FFmpeg `avformat_open_input` 读取媒体文件的流信息，
//! 不进行解码，仅提取元数据。

use crate::decoder::{decoded_video_range_from_ffmpeg, DecodedVideoRange};
use ffmpeg_next as ffmpeg;
use mondrian_core::icc::parse_icc_display_profile;
use mondrian_core::types::*;
use mondrian_core::{
    VideoContentLightMetadata, VideoHdrChromaticity, VideoHdrMetadataPayload, VideoHdrRational,
    VideoIccProfileMetadata, VideoMasteringDisplayLuminance, VideoMasteringDisplayMetadata,
    VideoMasteringDisplayPrimaries,
};
use serde::{Deserialize, Serialize};
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

// ─── 视频编解码器 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
    Vp9,
    ProRes(ProResVariant),
    DnxHd,
    DnxHr,
    Cineform,
    Raw,
    Other(String),
}

/// Codec profile proven by the opened FFmpeg decoder context.
///
/// `Unknown` is an explicit absence of evidence. Callers must not infer a
/// profile from codec, bit depth, filename, or container extension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecProfile {
    #[default]
    Unknown,
    HevcMain,
    HevcMain10,
    HevcMainStillPicture,
    HevcRangeExtensions,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProResVariant {
    Proxy,
    Lt,
    Standard,
    Hq,
    R4444,
    R4444Xq,
}

// ─── 音频编解码器 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    Aac,
    Mp3,
    Flac,
    Pcm { bit_depth: u8 },
    Opus,
    Vorbis,
    Other(String),
}

// ─── 像素格式 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    Yuv420p,
    Yuv422p,
    Yuv444p,
    Yuv420p10le,
    Yuv422p10le,
    Yuv444p10le,
    Rgb24,
    Rgba,
    Nv12, // GPU 硬解常见格式
    P010, // 10-bit NV12
}

impl PixelFormat {
    pub fn bit_depth(self) -> u8 {
        match self {
            Self::Yuv420p10le | Self::Yuv422p10le | Self::Yuv444p10le | Self::P010 => 10,
            _ => 8,
        }
    }

    pub fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba)
    }
}

/// How a video stream's input color metadata was resolved by media probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorSpaceSource {
    /// Media evidence identified or inferred the color space.
    Metadata,
    /// No color metadata was present; callers must apply missing-metadata policy.
    MissingMetadata,
    /// Color metadata was present but did not describe a supported product color space.
    UnsupportedMetadata,
    /// FFmpeg could not open a decoder; callers must apply missing-metadata policy.
    DecoderUnavailable,
}

/// Method that produced a video stream's color metadata decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorDetectionMethod {
    /// An acquisition/log metadata hint won; confidence distinguishes declarations from inference.
    MetadataHint,
    /// An embedded ICC profile identified the input color family.
    IccProfile,
    /// Raw CICP/FFmpeg color tags resolved to a supported color space.
    CicpTags,
    /// No supported color metadata was found.
    MissingMetadata,
    /// CICP tags were present but did not resolve to one supported product color space.
    UnsupportedCicpTags,
    /// Decoder could not be opened, so no color metadata could be inspected.
    DecoderUnavailable,
}

/// Confidence level for an automatic video color interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorInterpretationConfidence {
    /// No supported interpretation was available.
    None,
    /// A complete transfer/gamut pair appeared only in descriptive text or a file name.
    Low,
    /// Partial CICP tags or a mapped ICC profile identified a likely color space.
    Medium,
    /// Exact CICP tags or a declared acquisition metadata field identified a supported color space.
    High,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IccColorProfileHint {
    mapping: mondrian_core::icc::IccColorSpaceMapping,
    profile_name: Option<String>,
}

/// One raw CICP-style color tag reported by FFmpeg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorTag {
    /// Numeric CICP/FFmpeg enum value.
    pub code: i32,
    /// Stable FFmpeg tag name when FFmpeg exposes one.
    pub name: Option<String>,
    /// Whether this tag carries an explicit value rather than `unspecified`.
    pub specified: bool,
}

/// Raw container/codec color metadata for a decoded video stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorMetadata {
    /// Color primaries tag.
    pub primaries: VideoColorTag,
    /// Transfer characteristic tag.
    pub transfer: VideoColorTag,
    /// Matrix coefficients tag.
    pub matrix: VideoColorTag,
}

/// Evidence that contributed to a video color interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorInterpretationEvidence {
    /// A container or stream metadata hint identified an acquisition/log color space.
    MetadataHint {
        /// Scope where the hint was found.
        scope: VideoColorMetadataHintScope,
        /// Original metadata key.
        key: String,
        /// Original metadata value.
        value: String,
        /// Color space identified by the hint.
        detected_color_space: ColorSpace,
    },
    /// Complete CICP/FFmpeg primaries and transfer tags matched a supported RGB identity.
    ExactCicpTags {
        /// Color primaries tag.
        primaries: VideoColorTag,
        /// Transfer characteristic tag.
        transfer: VideoColorTag,
        /// Matrix coefficients tag.
        matrix: VideoColorTag,
        /// Color space identified by the exact triplet.
        detected_color_space: ColorSpace,
    },
    /// Partial CICP/FFmpeg tags identified a likely supported delivery space.
    PartialCicpTags {
        /// Color primaries tag.
        primaries: VideoColorTag,
        /// Transfer characteristic tag.
        transfer: VideoColorTag,
        /// Matrix coefficients tag.
        matrix: VideoColorTag,
        /// Color space identified by partial tags.
        detected_color_space: ColorSpace,
    },
    /// CICP/FFmpeg tags were present but did not map to a supported Mondrian color space.
    UnsupportedCicpTags {
        /// Color primaries tag.
        primaries: VideoColorTag,
        /// Transfer characteristic tag.
        transfer: VideoColorTag,
        /// Matrix coefficients tag.
        matrix: VideoColorTag,
    },
    /// FFmpeg could not open the decoder, so no decoder-side color tags were available.
    DecoderUnavailable,
    /// An ICC profile was present in the media.
    IccProfile {
        /// Explicitly mapped color space, if supported.
        mapped_color_space: Option<ColorSpace>,
        /// ICC profile name, if available.
        profile_name: Option<String>,
    },
}

/// Warning emitted while interpreting video color metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorInterpretationWarning {
    /// Conflicting metadata hints were found; the highest-priority declaration was selected.
    MultipleMetadataHints {
        /// Selected metadata hint.
        selected: VideoColorMetadataHint,
        /// Ignored metadata hints, in probe order.
        ignored: Vec<VideoColorMetadataHint>,
    },
    /// A metadata hint took precedence over CICP tags that indicated another color space.
    MetadataHintOverridesCicpTags {
        /// Selected metadata hint.
        selected: VideoColorMetadataHint,
        /// CICP-selected color space.
        cicp_color_space: ColorSpace,
        /// Raw CICP metadata that conflicted with the selected hint.
        cicp_metadata: VideoColorMetadata,
    },
    /// A free-form metadata field was used only because no stronger color evidence existed.
    DescriptiveMetadataHintInference {
        /// Low-confidence hint selected as the fallback interpretation.
        selected: VideoColorMetadataHint,
    },
    /// Stronger structured evidence won over conflicting free-form metadata hints.
    LowerPriorityMetadataHints {
        /// Detection method that supplied the selected interpretation.
        selected_method: VideoColorDetectionMethod,
        /// Color space selected by the stronger evidence.
        selected_color_space: ColorSpace,
        /// Conflicting metadata hints that were retained but not selected.
        ignored: Vec<VideoColorMetadataHint>,
    },
    /// The result came from partial CICP tags instead of a complete exact triplet.
    PartialCicpTags {
        /// Color space inferred from partial tags.
        detected_color_space: ColorSpace,
    },
    /// No CICP tags were present.
    MissingCicpTags,
    /// CICP tags were present but did not resolve to a supported product color space.
    UnsupportedCicpTags,
    /// Decoder metadata was unavailable.
    DecoderUnavailable,
    /// ICC profile parsed but could not be mapped to a supported OCIO identity.
    IccProfileUnmapped {
        /// ICC profile name, if available.
        profile_name: Option<String>,
        /// Structured mapping failure reason.
        reason: String,
    },
    /// ICC profile color space conflicts with CICP-detected color space.
    IccCicpMismatch {
        /// Color space inferred from ICC profile.
        icc_color_space: ColorSpace,
        /// Color space detected from CICP tags.
        cicp_color_space: ColorSpace,
    },
}

/// Structured interpretation of a video stream's input color metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedColorInterpretation {
    /// Explicitly detected Mondrian color space, if one was identified.
    pub color_space: Option<ColorSpace>,
    /// Confidence of the automatic interpretation.
    pub confidence: VideoColorInterpretationConfidence,
    /// Source state for the color metadata decision.
    pub source: VideoColorSpaceSource,
    /// Method that produced the color metadata decision.
    pub method: VideoColorDetectionMethod,
    /// Evidence used to make the interpretation.
    #[serde(default)]
    pub evidence: Vec<VideoColorInterpretationEvidence>,
    /// Non-fatal warnings describing ambiguity or missing evidence.
    #[serde(default)]
    pub warnings: Vec<VideoColorInterpretationWarning>,
    /// Whether the interpretation is designed to be overridden by the user.
    pub user_overridable: bool,
}

/// Scope where an acquisition/color metadata hint was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorMetadataHintScope {
    /// Container-level metadata.
    Container,
    /// Video stream-level metadata.
    Stream,
    /// Complete transfer-and-gamut pair parsed from the source file name.
    FileName,
}

/// Metadata hint that identifies an acquisition or camera-log color space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorMetadataHint {
    /// Metadata scope.
    pub scope: VideoColorMetadataHintScope,
    /// Original metadata key.
    pub key: String,
    /// Original metadata value.
    pub value: String,
    /// Color space identified by the hint.
    pub detected_color_space: ColorSpace,
}

/// HDR-related stream side-data kind detected during media probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoHdrSideDataKind {
    /// SMPTE ST 2086 mastering display metadata.
    MasteringDisplayMetadata,
    /// MaxCLL / MaxFALL content light level metadata.
    ContentLightLevel,
    /// HDR10+ dynamic metadata.
    DynamicHdr10Plus,
    /// Dolby Vision configuration metadata.
    DolbyVisionConfig,
    /// ICC profile side data.
    IccProfile,
}

/// Summary of HDR-related side data on a video stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoHdrMetadataSummary {
    /// HDR side-data kind.
    pub kind: VideoHdrSideDataKind,
    /// Side-data payload size in bytes.
    pub payload_size: usize,
    /// Parsed HDR metadata payload when FFmpeg exposes a stable ABI for the side data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<VideoHdrMetadataPayload>,
}

/// Diagnostic snapshot of a video stream's color metadata interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorDiagnostic {
    /// Explicitly detected Mondrian color space, if one was identified.
    pub detected_color_space: Option<ColorSpace>,
    /// Encoded quantization range reported by FFmpeg.
    #[serde(default)]
    pub color_range: DecodedVideoRange,
    /// Structured interpretation of automatic color metadata.
    pub interpretation: DetectedColorInterpretation,
    /// Source state for the color metadata decision.
    pub source: VideoColorSpaceSource,
    /// Method that produced the color metadata decision.
    pub method: VideoColorDetectionMethod,
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
    /// Explicitly detected Mondrian color space, if one was identified.
    pub detected_color_space: Option<ColorSpace>,
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
    pub diagnostics_with_detected_color_space: u64,
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

impl VideoColorTag {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        let name = self.name.as_deref().unwrap_or("unspecified");
        format!("{name}(code={},specified={})", self.code, self.specified)
    }
}

impl VideoColorMetadata {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        format!(
            "primaries={},transfer={},matrix={}",
            self.primaries.summary(),
            self.transfer.summary(),
            self.matrix.summary()
        )
    }
}

impl VideoColorDiagnostic {
    /// Build a color diagnostic snapshot from probed stream metadata.
    pub fn from_stream(stream: &VideoStreamInfo) -> Self {
        Self {
            detected_color_space: stream.detected_color_space,
            color_range: stream.color_range,
            interpretation: stream.color_interpretation.clone(),
            source: stream.color_space_source,
            method: stream.color_detection_method,
            metadata: stream.color_metadata.clone(),
            metadata_hints: stream.color_metadata_hints.clone(),
            hdr_metadata: stream.hdr_metadata.clone(),
        }
    }

    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        let detected = self
            .detected_color_space
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
            "source={:?},method={:?},detected={},range={:?},confidence={:?},overridable={},warnings={},metadata={},hints={},hdr={}",
            self.source,
            self.method,
            detected,
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
            detected_color_space: self.detected_color_space,
            confidence: self.interpretation.confidence,
            source: self.source,
            method: self.method,
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
        self.diagnostics_with_detected_color_space = self
            .diagnostics_with_detected_color_space
            .saturating_add(u64::from(summary.detected_color_space.is_some()));
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

impl VideoColorMetadataHint {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        format!(
            "{:?}:{}={}->{:?}",
            self.scope, self.key, self.value, self.detected_color_space
        )
    }
}

impl VideoColorInterpretationWarning {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        match self {
            Self::MultipleMetadataHints { selected, ignored } => {
                let ignored_summary = if ignored.is_empty() {
                    "none".to_string()
                } else {
                    ignored
                        .iter()
                        .map(VideoColorMetadataHint::summary)
                        .collect::<Vec<_>>()
                        .join("|")
                };
                format!(
                    "multiple_hints(selected={},ignored={})",
                    selected.summary(),
                    ignored_summary
                )
            }
            Self::MetadataHintOverridesCicpTags { selected, cicp_color_space, cicp_metadata } => {
                format!(
                    "hint_overrides_cicp(selected={},cicp={:?},metadata={})",
                    selected.summary(),
                    cicp_color_space,
                    cicp_metadata.summary()
                )
            }
            Self::DescriptiveMetadataHintInference { selected } => {
                format!(
                    "descriptive_hint_inference(selected={})",
                    selected.summary()
                )
            }
            Self::LowerPriorityMetadataHints { selected_method, selected_color_space, ignored } => {
                let ignored = ignored
                    .iter()
                    .map(VideoColorMetadataHint::summary)
                    .collect::<Vec<_>>()
                    .join("|");
                format!(
                    "lower_priority_hints(selected_method={selected_method:?},selected={selected_color_space:?},ignored={ignored})"
                )
            }
            Self::PartialCicpTags { detected_color_space } => {
                format!("partial_cicp(detected={detected_color_space:?})")
            }
            Self::MissingCicpTags => "missing_cicp".to_string(),
            Self::UnsupportedCicpTags => "unsupported_cicp".to_string(),
            Self::DecoderUnavailable => "decoder_unavailable".to_string(),
            Self::IccCicpMismatch { icc_color_space, cicp_color_space } => {
                format!("icc_cicp_mismatch(icc={icc_color_space:?},cicp={cicp_color_space:?})")
            }
            Self::IccProfileUnmapped { profile_name, reason } => {
                format!("icc_profile_unmapped(profile={profile_name:?},reason={reason})")
            }
        }
    }
}

impl VideoHdrMetadataSummary {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        let payload = self
            .payload
            .as_ref()
            .map(VideoHdrMetadataPayload::summary)
            .unwrap_or_else(|| "unparsed".to_string());
        format!(
            "{:?}(bytes={},payload={})",
            self.kind, self.payload_size, payload
        )
    }
}

// ─── 声道布局 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelLayout {
    Mono,
    Stereo,
    Surround51,
    Surround71,
    Other(u8),
}

// ─── 流信息 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub index: u32,
    pub codec: VideoCodec,
    /// Decoder-proven codec profile, or explicit `Unknown`.
    #[serde(default)]
    pub codec_profile: VideoCodecProfile,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    /// Whether `frame_rate` came from a positive FFmpeg stream rational.
    ///
    /// Container time-base quantization within 100 ppm of a standard nominal
    /// rate is canonicalized to that exact rational.
    #[serde(default)]
    pub frame_rate_proven: bool,
    pub pixel_format: PixelFormat,
    /// Whether FFmpeg reported this exact pixel format rather than a storage fallback.
    #[serde(default)]
    pub pixel_format_proven: bool,
    /// Encoded quantization range reported by FFmpeg for the primary decoder.
    #[serde(default)]
    pub color_range: DecodedVideoRange,
    /// Color space explicitly detected from container/codec metadata, if present.
    pub detected_color_space: Option<ColorSpace>,
    /// Structured automatic color interpretation with evidence and warnings.
    pub color_interpretation: DetectedColorInterpretation,
    /// Source of the detected color-space result.
    pub color_space_source: VideoColorSpaceSource,
    /// Method that produced the detected color-space result.
    pub color_detection_method: VideoColorDetectionMethod,
    /// Raw CICP-style color metadata reported by FFmpeg when the decoder opens.
    pub color_metadata: Option<VideoColorMetadata>,
    /// Acquisition/log hints captured from stream, container, or complete file-name pairs.
    #[serde(default)]
    pub color_metadata_hints: Vec<VideoColorMetadataHint>,
    /// HDR-related stream side-data summaries.
    #[serde(default)]
    pub hdr_metadata: Vec<VideoHdrMetadataSummary>,
    pub bit_depth: u8,
    pub has_alpha: bool,
    pub avg_bitrate: u64, // bits/s
    pub total_frames: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub index: u32,
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u8,
    pub channel_layout: ChannelLayout,
    pub bit_depth: u16,
    pub avg_bitrate: u64,
}

// ─── MediaInfo ────────────────────────────────────────────────────────────────

/// 媒体文件完整元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub duration: Duration,
    pub file_size: u64,
    pub container: String, // "mp4", "mov", "mkv", ...
    pub video_streams: Vec<VideoStreamInfo>,
    pub audio_streams: Vec<AudioStreamInfo>,
    /// 是否有视频流
    pub has_video: bool,
    /// 是否有音频流
    pub has_audio: bool,
}

impl MediaInfo {
    pub fn synthetic_adjustment_layer() -> Self {
        Self {
            path: PathBuf::from("mondrian://adjustment-layer"),
            duration: Duration::ZERO,
            file_size: 0,
            container: "adjustment-layer".to_string(),
            video_streams: Vec::new(),
            audio_streams: Vec::new(),
            has_video: false,
            has_audio: false,
        }
    }

    pub fn synthetic_solid_color() -> Self {
        Self {
            path: PathBuf::from("mondrian://solid-color"),
            duration: Duration::ZERO,
            file_size: 0,
            container: "solid-color".to_string(),
            video_streams: Vec::new(),
            audio_streams: Vec::new(),
            has_video: false,
            has_audio: false,
        }
    }

    /// 探针媒体文件（同步，通过 FFmpeg AVFormatContext）
    ///
    /// # 注意
    /// 此函数在后台线程调用，避免阻塞 UI 线程。
    pub fn probe(path: &Path) -> mondrian_core::Result<Self> {
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
                    let mut color_metadata_hints = collect_color_metadata_hints(
                        VideoColorMetadataHintScope::Stream,
                        &stream.metadata(),
                    );
                    color_metadata_hints.extend(container_color_hints.clone());
                    color_metadata_hints.extend(file_name_color_hint.clone());
                    let hdr_metadata = collect_hdr_metadata_summaries(&stream);
                    let mut width = 0;
                    let mut height = 0;
                    let mut pixel_format = PixelFormat::Yuv420p;
                    let mut pixel_format_proven = false;
                    let mut bit_depth = 8;
                    let mut has_alpha = false;

                    if let Ok(context) =
                        ffmpeg::codec::context::Context::from_parameters(params.clone())
                    {
                        if let Ok(decoder) = context.decoder().video() {
                            width = decoder.width();
                            height = decoder.height();
                            if let Some(probed_pixel_format) = map_pixel_format(decoder.format()) {
                                pixel_format = probed_pixel_format;
                                pixel_format_proven = true;
                            }
                            let color_range =
                                decoded_video_range_from_ffmpeg(decoder.color_range());
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
                            );
                            let detected_color_space = color_interpretation.color_space;
                            let color_space_source = color_interpretation.source;
                            let color_detection_method = color_interpretation.method;
                            let (frame_rate, frame_rate_proven) =
                                map_rational(stream.avg_frame_rate());
                            video_streams.push(VideoStreamInfo {
                                index: stream.index() as u32,
                                codec: map_video_codec(params.id()),
                                codec_profile: map_video_codec_profile(decoder.profile()),
                                width,
                                height,
                                frame_rate,
                                frame_rate_proven,
                                pixel_format,
                                pixel_format_proven,
                                color_range,
                                detected_color_space,
                                color_interpretation,
                                color_space_source,
                                color_detection_method,
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
                        codec_profile: VideoCodecProfile::Unknown,
                        width,
                        height,
                        frame_rate,
                        frame_rate_proven,
                        pixel_format,
                        pixel_format_proven,
                        color_range: DecodedVideoRange::Unknown,
                        detected_color_space: None,
                        color_interpretation: DetectedColorInterpretation::decoder_unavailable(),
                        color_space_source: VideoColorSpaceSource::DecoderUnavailable,
                        color_detection_method: VideoColorDetectionMethod::DecoderUnavailable,
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
                    let mut sample_rate = 0u32;
                    let mut channels = 0u8;
                    let mut channel_layout = ChannelLayout::Other(0);
                    let mut bit_depth = 16u16;

                    if let Ok(context) =
                        ffmpeg::codec::context::Context::from_parameters(params.clone())
                    {
                        if let Ok(decoder) = context.decoder().audio() {
                            sample_rate = decoder.rate();
                            channels = decoder.channels() as u8;
                            channel_layout = map_channel_layout(channels);
                            bit_depth = 16;
                        }
                    }

                    audio_streams.push(AudioStreamInfo {
                        index: stream.index() as u32,
                        codec: map_audio_codec(params.id()),
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

        let info = Self {
            path: path.to_path_buf(),
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

    /// 获取主要视频流（第一个视频流）
    pub fn primary_video(&self) -> Option<&VideoStreamInfo> {
        self.video_streams.first()
    }

    /// 获取主要音频流（第一个音频流）
    pub fn primary_audio(&self) -> Option<&AudioStreamInfo> {
        self.audio_streams.first()
    }

    /// 获取视频总帧数（估算）
    pub fn estimated_frames(&self) -> Option<u64> {
        let video = self.primary_video()?;
        let fps = video.frame_rate.to_f64();
        let secs = self.duration.as_secs_f64();
        Some((secs * fps).ceil() as u64)
    }
}

#[cfg(test)]
fn detect_color_space(
    primaries: ffmpeg::util::color::Primaries,
    transfer: ffmpeg::util::color::TransferCharacteristic,
    matrix: ffmpeg::util::color::Space,
) -> DetectedColorInterpretation {
    let metadata = capture_color_metadata(primaries, transfer, matrix);
    detect_color_space_from_metadata(&metadata, &[], None)
}

fn detect_color_space_from_metadata(
    metadata: &VideoColorMetadata,
    metadata_hints: &[VideoColorMetadataHint],
    icc_profile: Option<&IccColorProfileHint>,
) -> DetectedColorInterpretation {
    let exact_cicp = exact_cicp_color_space(metadata);
    let hinted_cicp = ColorSpace::from_ffmpeg_tag_hints(
        metadata.primaries.name.as_deref(),
        metadata.transfer.name.as_deref(),
        metadata.matrix.name.as_deref(),
    );
    let cicp_color_space = exact_cicp.or(hinted_cicp);
    let unresolved_cicp = UnresolvedCicpMetadata::from_metadata(metadata);

    if let Some((selected_index, selected_hint)) = metadata_hints
        .iter()
        .enumerate()
        .filter(|(_, hint)| metadata_hint_is_explicit_declaration(hint))
        .min_by_key(|(index, hint)| (metadata_hint_scope_priority(hint.scope), *index))
    {
        let color_space = selected_hint.detected_color_space;
        let mut interpretation = DetectedColorInterpretation {
            color_space: Some(color_space),
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
        let conflicting_hints = metadata_hints
            .iter()
            .enumerate()
            .filter(|(index, hint)| {
                *index != selected_index && hint.detected_color_space != color_space
            })
            .map(|(_, hint)| hint.clone())
            .collect::<Vec<_>>();
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
            color_space: Some(color_space),
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
            color_space: Some(color_space),
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
                color_space: Some(color_space),
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
                color_space: None,
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

    let mut evidence = Vec::new();
    unresolved_cicp.append_evidence(&mut evidence, metadata);
    let mut interpretation = DetectedColorInterpretation {
        color_space: None,
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
        color_space: Some(selected_hint.detected_color_space),
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
/// CICP tags and acquisition metadata hints. Missing metadata remains unresolved;
/// the timeline's missing-metadata policy owns any later assumption.
pub fn interpret_video_color_metadata(
    metadata: &VideoColorMetadata,
    metadata_hints: &[VideoColorMetadataHint],
) -> DetectedColorInterpretation {
    detect_color_space_from_metadata(metadata, metadata_hints, None)
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

impl DetectedColorInterpretation {
    fn decoder_unavailable() -> Self {
        Self {
            color_space: None,
            confidence: VideoColorInterpretationConfidence::None,
            source: VideoColorSpaceSource::DecoderUnavailable,
            method: VideoColorDetectionMethod::DecoderUnavailable,
            evidence: vec![VideoColorInterpretationEvidence::DecoderUnavailable],
            warnings: vec![VideoColorInterpretationWarning::DecoderUnavailable],
            user_overridable: true,
        }
    }
}

fn exact_cicp_color_space(metadata: &VideoColorMetadata) -> Option<ColorSpace> {
    ColorSpace::from_ffmpeg_colorimetry(
        metadata.primaries.name.as_deref()?,
        metadata.transfer.name.as_deref()?,
    )
}

fn metadata_hint_evidence(hint: &VideoColorMetadataHint) -> VideoColorInterpretationEvidence {
    VideoColorInterpretationEvidence::MetadataHint {
        scope: hint.scope,
        key: hint.key.clone(),
        value: hint.value.clone(),
        detected_color_space: hint.detected_color_space,
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

fn metadata_hint_is_explicit_declaration(hint: &VideoColorMetadataHint) -> bool {
    const DECLARATION_KEY_SUFFIXES: [&str; 9] = [
        "cameraprofile",
        "cameralog",
        "cameracolorspace",
        "colorprofile",
        "colorspace",
        "gammagamut",
        "inputcolorspace",
        "logprofile",
        "sourcecolorspace",
    ];
    let key = normalize_metadata_hint_text(&hint.key);
    DECLARATION_KEY_SUFFIXES.iter().any(|suffix| key.ends_with(suffix))
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
    })
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
    use ffmpeg::codec::packet::side_data::Type;

    match kind {
        Type::MasteringDisplayMetadata => {
            parse_mastering_display_payload(data).map(VideoHdrMetadataPayload::MasteringDisplay)
        }
        Type::ContentLightLevel => {
            parse_content_light_payload(data).map(VideoHdrMetadataPayload::ContentLightLevel)
        }
        Type::ICC_PROFILE => parse_icc_display_profile(data)
            .ok()
            .map(|profile| VideoIccProfileMetadata { name: profile.name, mapping: profile.mapping })
            .map(VideoHdrMetadataPayload::IccProfile),
        _ => None,
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
    use ffmpeg::codec::profile::HEVC;
    use ffmpeg::codec::Profile;

    match profile {
        Profile::Unknown | Profile::Reserved => VideoCodecProfile::Unknown,
        Profile::HEVC(HEVC::Main) => VideoCodecProfile::HevcMain,
        Profile::HEVC(HEVC::Main10) => VideoCodecProfile::HevcMain10,
        Profile::HEVC(HEVC::MainStillPicture) => VideoCodecProfile::HevcMainStillPicture,
        Profile::HEVC(HEVC::Rext) => VideoCodecProfile::HevcRangeExtensions,
        _ => VideoCodecProfile::Other,
    }
}

fn map_channel_layout(channels: u8) -> ChannelLayout {
    match channels {
        1 => ChannelLayout::Mono,
        2 => ChannelLayout::Stereo,
        6 => ChannelLayout::Surround51,
        8 => ChannelLayout::Surround71,
        _ => ChannelLayout::Other(channels),
    }
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
    fn probe_mapping_preserves_exact_hevc_main10_profile() {
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
        assert_eq!(pq.color_space, Some(ColorSpace::Rec2100Pq));
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
        assert_eq!(hlg.color_space, Some(ColorSpace::Rec2100Hlg));
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
        let detection = detect_color_space_from_metadata(&metadata, &[], None);

        assert_eq!(detection.color_space, None);
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
            detected_color_space: detection.color_space,
            color_range: DecodedVideoRange::Full,
            source: detection.source,
            method: detection.method,
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

        let detection = interpret_video_color_metadata(&metadata, std::slice::from_ref(&hint));

        assert_eq!(detection.color_space, Some(ColorSpace::ArriLogC4WideGamut4));
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

        assert_eq!(detection.color_space, None);
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

        assert_eq!(detection.color_space, Some(ColorSpace::Rec709));
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
    }

    #[test]
    fn detect_color_space_recognizes_exact_pal_and_ntsc_rec601_tags() {
        let pal = detect_color_space(
            Primaries::BT470BG,
            TransferCharacteristic::GAMMA28,
            Space::BT470BG,
        );
        assert_eq!(pal.color_space, Some(ColorSpace::Rec601Pal));
        assert_eq!(pal.confidence, VideoColorInterpretationConfidence::High);

        let ntsc = detect_color_space(
            Primaries::SMPTE170M,
            TransferCharacteristic::SMPTE170M,
            Space::SMPTE170M,
        );
        assert_eq!(ntsc.color_space, Some(ColorSpace::Rec601Ntsc));
        assert_eq!(ntsc.confidence, VideoColorInterpretationConfidence::High);
    }

    #[test]
    fn detect_color_space_uses_matrix_metadata_when_primaries_are_missing() {
        let detection = detect_color_space(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::BT709,
        );

        assert_eq!(detection.color_space, Some(ColorSpace::Rec709));
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Medium
        );
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
    }

    #[test]
    fn detect_color_space_does_not_claim_missing_metadata_as_rec709() {
        let detection = detect_color_space(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );

        assert_eq!(detection.color_space, None);
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

        let detection = detect_color_space_from_metadata(&metadata, &[], Some(&icc_profile));

        assert_eq!(detection.color_space, Some(ColorSpace::DisplayP3));
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Medium
        );
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::IccProfile);
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
            value: "converted from S-Log3 / S-Gamut3.Cine".to_string(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
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
        );

        assert_eq!(detection.color_space, Some(ColorSpace::DisplayP3));
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

        let detection = detect_color_space_from_metadata(&metadata, &[], Some(&icc_profile));

        assert_eq!(detection.color_space, None);
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
            detected_color_space: detection.color_space,
            color_range: DecodedVideoRange::Unknown,
            source: detection.source,
            method: detection.method,
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
        );

        assert_eq!(
            detection.color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Low
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

        let detection = detect_color_space_from_metadata(&metadata, &[], Some(&icc_profile));

        assert_eq!(detection.color_space, Some(ColorSpace::Rec709));
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

        let apple_log = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Container,
            "com.apple.proapps.cameraLog",
            "Apple Log",
        )
        .expect("apple log metadata hint");
        assert_eq!(apple_log.detected_color_space, ColorSpace::AppleLogBt2020);

        let logc4 = parse_video_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "camera_profile",
            "ARRI LogC4",
        )
        .expect("arri logc4 metadata hint");
        assert_eq!(logc4.detected_color_space, ColorSpace::ArriLogC4WideGamut4);
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

        let detection = interpret_video_color_metadata(&metadata, std::slice::from_ref(&hint));

        assert_eq!(
            detection.color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::High
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

        let detection = interpret_video_color_metadata(&metadata, std::slice::from_ref(&hint));

        assert_eq!(
            detection.color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Low
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
    fn metadata_hint_overrides_cicp_delivery_tags_for_camera_log() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let hint = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "camera_profile".to_string(),
            value: "ARRI LogC4".to_string(),
            detected_color_space: ColorSpace::ArriLogC4WideGamut4,
        };

        let detection =
            detect_color_space_from_metadata(&metadata, std::slice::from_ref(&hint), None);

        assert_eq!(detection.color_space, Some(ColorSpace::ArriLogC4WideGamut4));
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::High
        );
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::MetadataHint);
        assert!(matches!(
            detection.evidence.first(),
            Some(VideoColorInterpretationEvidence::MetadataHint {
                detected_color_space: ColorSpace::ArriLogC4WideGamut4,
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
        };

        let detection = interpret_video_color_metadata(&metadata, std::slice::from_ref(&hint));

        assert_eq!(detection.color_space, Some(ColorSpace::Rec709));
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
            detected_color_space: detection.color_space,
            color_range: DecodedVideoRange::Unknown,
            interpretation: detection.clone(),
            source: detection.source,
            method: detection.method,
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
        };

        let detection = interpret_video_color_metadata(&metadata, std::slice::from_ref(&hint));

        assert_eq!(
            detection.color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::Low
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
        };
        let declared = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "camera_profile".to_string(),
            value: "ARRI LogC4".to_string(),
            detected_color_space: ColorSpace::ArriLogC4WideGamut4,
        };

        let detection =
            interpret_video_color_metadata(&metadata, &[descriptive.clone(), declared.clone()]);

        assert_eq!(detection.color_space, Some(ColorSpace::ArriLogC4WideGamut4));
        assert_eq!(
            detection.confidence,
            VideoColorInterpretationConfidence::High
        );
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::MultipleMetadataHints {
                selected: declared,
                ignored: vec![descriptive],
            }
        ));
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
        };
        let stream = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "camera_profile".to_string(),
            value: "ARRI LogC4".to_string(),
            detected_color_space: ColorSpace::ArriLogC4WideGamut4,
        };

        let detection =
            interpret_video_color_metadata(&metadata, &[container.clone(), stream.clone()]);

        assert_eq!(detection.color_space, Some(ColorSpace::ArriLogC4WideGamut4));
        assert!(detection.warnings.contains(
            &VideoColorInterpretationWarning::MultipleMetadataHints {
                selected: stream,
                ignored: vec![container],
            }
        ));
    }

    #[test]
    fn multiple_metadata_hints_keep_selected_and_ignored_evidence() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );
        let hints = vec![
            VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Stream,
                key: "camera_profile".to_string(),
                value: "S-Log3 / S-Gamut3.Cine".to_string(),
                detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            },
            VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Container,
                key: "camera_profile".to_string(),
                value: "Apple Log".to_string(),
                detected_color_space: ColorSpace::AppleLogBt2020,
            },
        ];

        let detection = detect_color_space_from_metadata(&metadata, &hints, None);

        assert_eq!(
            detection.color_space,
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
        let hint = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "camera_profile".to_string(),
            value: "ARRI LogC4".to_string(),
            detected_color_space: ColorSpace::ArriLogC4WideGamut4,
        };
        let interpretation =
            detect_color_space_from_metadata(&metadata, std::slice::from_ref(&hint), None);
        let diagnostic = VideoColorDiagnostic {
            detected_color_space: interpretation.color_space,
            color_range: DecodedVideoRange::Limited,
            interpretation,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
            metadata: Some(metadata),
            metadata_hints: vec![hint],
            hdr_metadata: Vec::new(),
        };

        let summary = diagnostic.summary();

        assert!(summary.contains("hint_overrides_cicp"));
        assert!(summary.contains("Stream:camera_profile=ARRI LogC4->ArriLogC4WideGamut4"));
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
            VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Stream,
                key: "camera_profile".to_string(),
                value: "ARRI LogC4".to_string(),
                detected_color_space: ColorSpace::ArriLogC4WideGamut4,
            },
            VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Container,
                key: "camera_profile".to_string(),
                value: "S-Log3 / S-Gamut3.Cine".to_string(),
                detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            },
        ];
        let interpretation = detect_color_space_from_metadata(&metadata, &hints, None);
        let diagnostic = VideoColorDiagnostic {
            detected_color_space: interpretation.color_space,
            color_range: DecodedVideoRange::Limited,
            interpretation,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
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
                detected_color_space: Some(ColorSpace::ArriLogC4WideGamut4),
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
            detected_color_space: missing_interpretation.color_space,
            color_range: DecodedVideoRange::Unknown,
            interpretation: missing_interpretation,
            source: VideoColorSpaceSource::MissingMetadata,
            method: VideoColorDetectionMethod::MissingMetadata,
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        };
        let missing_summary = missing_diagnostic.issue_summary();

        assert_eq!(missing_summary.detected_color_space, None);
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
            detected_color_space: unavailable_interpretation.color_space,
            color_range: DecodedVideoRange::Unknown,
            interpretation: unavailable_interpretation,
            source: VideoColorSpaceSource::DecoderUnavailable,
            method: VideoColorDetectionMethod::DecoderUnavailable,
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
            VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Stream,
                key: "camera_profile".to_string(),
                value: "ARRI LogC4".to_string(),
                detected_color_space: ColorSpace::ArriLogC4WideGamut4,
            },
            VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Container,
                key: "camera_profile".to_string(),
                value: "S-Log3 / S-Gamut3.Cine".to_string(),
                detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            },
        ];
        let metadata_interpretation = detect_color_space_from_metadata(&metadata, &hints, None);
        let metadata_diagnostic = VideoColorDiagnostic {
            detected_color_space: metadata_interpretation.color_space,
            color_range: DecodedVideoRange::Limited,
            interpretation: metadata_interpretation,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
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
            detected_color_space: unavailable_interpretation.color_space,
            color_range: DecodedVideoRange::Unknown,
            interpretation: unavailable_interpretation,
            source: VideoColorSpaceSource::DecoderUnavailable,
            method: VideoColorDetectionMethod::DecoderUnavailable,
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
                diagnostics_with_detected_color_space: 1,
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
            detected_color_space: Some(ColorSpace::Rec2100Pq),
            color_range: DecodedVideoRange::Limited,
            interpretation: detect_color_space_from_metadata(&metadata, &[], None),
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::CicpTags,
            metadata: Some(metadata),
            metadata_hints: vec![VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Stream,
                key: "camera_profile".to_string(),
                value: "S-Log3 / S-Gamut3.Cine".to_string(),
                detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
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
        assert!(summary.contains("detected=Rec2100Pq"));
        assert!(summary.contains("range=Limited"));
        assert!(summary.contains("confidence=High"));
        assert!(summary.contains("overridable=true"));
        assert!(summary.contains("primaries=bt2020"));
        assert!(summary.contains("transfer=smpte2084"));
        assert!(summary.contains("matrix=bt2020nc"));
        assert!(summary.contains("camera_profile=S-Log3 / S-Gamut3.Cine"));
        assert!(summary.contains("MasteringDisplayMetadata(bytes=88,payload=master_display"));
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
