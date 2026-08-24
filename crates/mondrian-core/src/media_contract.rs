//! Stable media-source evidence shared by authoring and execution Modules.
//!
//! These values are immutable probe facts. They deliberately contain no
//! FFmpeg handles or probing implementation, so persisted Asset Library state
//! does not depend on the concrete media Adapter.

use crate::{
    AudioChannelLayout, ColorSpace, PictureStreamMetadata, Rational, VideoHdrMetadataPayload,
    MAX_AUDIO_CHANNELS,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

/// Stable identity of one filesystem object.
///
/// This value names the opened file object rather than its path. It is useful
/// only together with [`MediaFileChangeStamp`] and the observed size and
/// modification time; none of those facts alone authorizes reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaFileObjectIdentity {
    /// Windows volume serial number plus the filesystem's 128-bit file ID.
    Windows {
        /// Volume serial number returned for the opened handle.
        volume_serial_number: u64,
        /// Filesystem object ID returned for the opened handle.
        file_id: [u8; 16],
    },
    /// Unix device and inode identity.
    Unix {
        /// Device containing the opened file.
        device: u64,
        /// Inode of the opened file.
        inode: u64,
    },
}

/// Filesystem-owned change generation for one opened file object.
///
/// Unlike modification time, this stamp also changes when callers restore a
/// previous mtime after replacing bytes or metadata on filesystems that expose
/// the required evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaFileChangeStamp {
    /// Windows `FILE_BASIC_INFO::ChangeTime`, in 100 ns units since 1601.
    WindowsFileTime(i64),
    /// Unix inode-change time.
    Unix {
        /// Whole seconds in the platform's native epoch.
        seconds: i64,
        /// Subsecond nanoseconds.
        nanoseconds: i64,
    },
}

/// Conservative filesystem evidence used as a media-source revision token.
///
/// This value is not a content hash or durable filesystem identity. A partial
/// value is evidence of an unsuccessful observation and never authorizes cache,
/// stream-binding, or Session reuse. Complete evidence binds one open-file
/// observation to its filesystem object identity and change generation, so a
/// same-length replacement or restored mtime cannot masquerade as the admitted
/// source revision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaFileFingerprint {
    /// File length in bytes when available.
    pub len: Option<u64>,
    /// File modification time seconds since Unix epoch when available.
    pub modified_secs: Option<u64>,
    /// File modification time subsecond nanoseconds when available.
    pub modified_nanos: Option<u32>,
    /// Identity of the opened filesystem object.
    #[serde(default)]
    pub object_identity: Option<MediaFileObjectIdentity>,
    /// Filesystem-owned change generation of that object.
    #[serde(default)]
    pub change_stamp: Option<MediaFileChangeStamp>,
}

impl MediaFileFingerprint {
    /// Whether this evidence can conservatively authorize reuse.
    pub const fn authorizes_reuse(self) -> bool {
        self.len.is_some()
            && self.modified_secs.is_some()
            && self.modified_nanos.is_some()
            && self.object_identity.is_some()
            && self.change_stamp.is_some()
    }

    /// Capture one internally consistent open-file revision observation.
    ///
    /// Unsupported filesystems or unavailable object/change evidence produce a
    /// partial value that deliberately fails [`Self::authorizes_reuse`].
    pub fn capture(path: &Path) -> Self {
        File::open(path)
            .ok()
            .and_then(|file| Self::from_open_file(&file))
            .unwrap_or_default()
    }

    /// Build portable metadata evidence the caller already fetched.
    ///
    /// On platforms where `Metadata` exposes object identity and change time,
    /// the result may authorize reuse. Windows callers need [`Self::capture`]
    /// because the change generation is available only from the open handle.
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        let modified =
            metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok());
        let mut fingerprint = Self {
            len: Some(metadata.len()),
            modified_secs: modified.map(|duration| duration.as_secs()),
            modified_nanos: modified.map(|duration| duration.subsec_nanos()),
            object_identity: None,
            change_stamp: None,
        };
        populate_metadata_revision_evidence(metadata, &mut fingerprint);
        fingerprint
    }

    fn from_open_file(file: &File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        let mut fingerprint = Self::from_metadata(&metadata);
        populate_open_file_revision_evidence(file, &mut fingerprint);
        Some(fingerprint)
    }
}

#[cfg(unix)]
fn populate_metadata_revision_evidence(
    metadata: &std::fs::Metadata,
    fingerprint: &mut MediaFileFingerprint,
) {
    use std::os::unix::fs::MetadataExt;

    fingerprint.object_identity =
        Some(MediaFileObjectIdentity::Unix { device: metadata.dev(), inode: metadata.ino() });
    fingerprint.change_stamp = Some(MediaFileChangeStamp::Unix {
        seconds: metadata.ctime(),
        nanoseconds: metadata.ctime_nsec(),
    });
}

#[cfg(not(unix))]
fn populate_metadata_revision_evidence(
    _metadata: &std::fs::Metadata,
    _fingerprint: &mut MediaFileFingerprint,
) {
}

#[cfg(windows)]
fn populate_open_file_revision_evidence(file: &File, fingerprint: &mut MediaFileFingerprint) {
    use std::{
        ffi::c_void,
        mem::{size_of, MaybeUninit},
        os::windows::io::AsRawHandle,
        ptr::null_mut,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
        FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
    let mut filesystem_name = [0_u16; 32];
    // SAFETY: the live file handle is valid for this query and the filesystem
    // name buffer is writable for the exact supplied extent.
    let filesystem_ok = unsafe {
        GetVolumeInformationByHandleW(
            handle,
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            filesystem_name.as_mut_ptr(),
            filesystem_name.len() as u32,
        ) != 0
    };
    let filesystem_name_end = filesystem_name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(filesystem_name.len());
    let filesystem_name = String::from_utf16_lossy(&filesystem_name[..filesystem_name_end]);
    if !filesystem_ok
        || !(filesystem_name.eq_ignore_ascii_case("NTFS")
            || filesystem_name.eq_ignore_ascii_case("ReFS"))
    {
        return;
    }
    let mut basic = MaybeUninit::<FILE_BASIC_INFO>::uninit();
    let mut identity = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: both calls receive the live file handle and correctly sized,
    // writable buffers for the requested information class. The buffers are
    // read only after the OS reports success.
    let (basic, identity) = unsafe {
        let basic_ok = GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            basic.as_mut_ptr().cast::<c_void>(),
            size_of::<FILE_BASIC_INFO>() as u32,
        ) != 0;
        let identity_ok = GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            identity.as_mut_ptr().cast::<c_void>(),
            size_of::<FILE_ID_INFO>() as u32,
        ) != 0;
        if !basic_ok || !identity_ok {
            return;
        }
        (basic.assume_init(), identity.assume_init())
    };
    if basic.ChangeTime <= 0 {
        return;
    }
    fingerprint.object_identity = Some(MediaFileObjectIdentity::Windows {
        volume_serial_number: identity.VolumeSerialNumber,
        file_id: identity.FileId.Identifier,
    });
    fingerprint.change_stamp = Some(MediaFileChangeStamp::WindowsFileTime(basic.ChangeTime));
}

#[cfg(not(windows))]
fn populate_open_file_revision_evidence(_file: &File, _fingerprint: &mut MediaFileFingerprint) {}

/// Encoded video codec identified by a media probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    /// H.264/AVC.
    H264,
    /// H.265/HEVC.
    H265,
    /// AV1.
    Av1,
    /// VP9.
    Vp9,
    /// Apple ProRes with a proven variant.
    ProRes(ProResVariant),
    /// Avid DNxHD.
    DnxHd,
    /// Avid DNxHR.
    DnxHr,
    /// GoPro CineForm.
    Cineform,
    /// Uncompressed/raw video.
    Raw,
    /// Codec not represented by this product version.
    Other(String),
}

/// ProRes codec variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProResVariant {
    /// ProRes Proxy.
    Proxy,
    /// ProRes LT.
    Lt,
    /// ProRes 422.
    Standard,
    /// ProRes 422 HQ.
    Hq,
    /// ProRes 4444.
    R4444,
    /// ProRes 4444 XQ.
    R4444Xq,
}

/// Decoder-proven video codec profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecProfile {
    /// Decoder did not prove a known profile.
    #[default]
    Unknown,
    /// H.264 constrained profile.
    H264Constrained,
    /// H.264 Intra profile family.
    H264Intra,
    /// H.264 Baseline.
    H264Baseline,
    /// H.264 Constrained Baseline.
    H264ConstrainedBaseline,
    /// H.264 Main.
    H264Main,
    /// H.264 Extended.
    H264Extended,
    /// H.264 High.
    H264High,
    /// H.264 High 10.
    H264High10,
    /// H.264 High 10 Intra.
    H264High10Intra,
    /// H.264 High 4:2:2.
    H264High422,
    /// H.264 High 4:2:2 Intra.
    H264High422Intra,
    /// H.264 High 4:4:4.
    H264High444,
    /// H.264 High 4:4:4 Predictive.
    H264High444Predictive,
    /// H.264 High 4:4:4 Intra.
    H264High444Intra,
    /// H.264 CAVLC 4:4:4 Intra.
    H264Cavlc444,
    /// HEVC Main.
    HevcMain,
    /// HEVC Main 10.
    HevcMain10,
    /// HEVC Main Still Picture.
    HevcMainStillPicture,
    /// HEVC Range Extensions family.
    HevcRangeExtensions,
    /// Proven profile not represented by this product version.
    Other,
}

/// Encoded audio codec identified by a media probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    /// Advanced Audio Coding.
    Aac,
    /// MPEG Layer III.
    Mp3,
    /// Free Lossless Audio Codec.
    Flac,
    /// Linear PCM with explicit sample bit depth.
    Pcm {
        /// Encoded sample bit depth reported by the probe.
        bit_depth: u8,
    },
    /// Opus.
    Opus,
    /// Vorbis.
    Vorbis,
    /// Codec not represented by this product version.
    Other(String),
}

/// Encoded pixel format identified by a media probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// Planar YUV 4:2:0, 8-bit.
    Yuv420p,
    /// Planar YUV 4:2:2, 8-bit.
    Yuv422p,
    /// Planar YUV 4:4:4, 8-bit.
    Yuv444p,
    /// Planar YUV 4:2:0, 10-bit little-endian.
    Yuv420p10le,
    /// Planar YUV 4:2:2, 10-bit little-endian.
    Yuv422p10le,
    /// Planar YUV 4:4:4, 10-bit little-endian.
    Yuv444p10le,
    /// Packed RGB24.
    Rgb24,
    /// Packed RGBA8.
    Rgba,
    /// Two-plane 8-bit NV12.
    Nv12,
    /// Two-plane 10-bit P010.
    P010,
}

impl PixelFormat {
    /// Nominal component bit depth.
    pub const fn bit_depth(self) -> u8 {
        match self {
            Self::Yuv420p10le | Self::Yuv422p10le | Self::Yuv444p10le | Self::P010 => 10,
            _ => 8,
        }
    }

    /// Whether the encoded format carries Alpha.
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba)
    }

    /// Whether samples are already encoded as RGB rather than YCbCr.
    pub const fn is_rgb(self) -> bool {
        matches!(self, Self::Rgb24 | Self::Rgba)
    }
}

/// Internally consistent sampling facts proven by one media probe.
///
/// Persisted probe snapshots retain conservative storage fallbacks so their
/// serialized shape remains stable when an Adapter cannot map a decoder pixel
/// format. Consumers must use [`VideoStreamInfo::proven_sampling`] instead of
/// interpreting those fallback fields directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenVideoSampling {
    /// Exact encoded pixel format.
    pub pixel_format: PixelFormat,
    /// Exact nominal component bit depth.
    pub bit_depth: u8,
    /// Whether the encoded format carries Alpha.
    pub has_alpha: bool,
}

/// Encoded quantization range reported by probe or decoder evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecodedVideoRange {
    /// Studio/legal encoded range.
    Limited,
    /// Full encoded range.
    Full,
    /// No reliable range evidence.
    #[default]
    Unknown,
}

/// How input color metadata was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorSpaceSource {
    /// Metadata resolved a supported input identity.
    Metadata,
    /// No usable color metadata was present.
    MissingMetadata,
    /// Metadata was present but unsupported.
    UnsupportedMetadata,
    /// Decoder metadata could not be inspected.
    DecoderUnavailable,
}

/// Method that produced an input color decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorDetectionMethod {
    /// Acquisition or camera metadata hint.
    MetadataHint,
    /// Embedded ICC profile.
    IccProfile,
    /// Complete or compatible CICP tags.
    CicpTags,
    /// No usable metadata.
    MissingMetadata,
    /// Present but unsupported CICP tags.
    UnsupportedCicpTags,
    /// Decoder could not be opened.
    DecoderUnavailable,
}

/// Confidence of an automatic color interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorInterpretationConfidence {
    /// No supported interpretation.
    None,
    /// Weak descriptive evidence.
    Low,
    /// Partial structured evidence.
    Medium,
    /// Exact or explicit structured evidence.
    High,
}

/// One raw CICP-style color tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorTag {
    /// Numeric CICP/decoder enumeration value.
    pub code: i32,
    /// Stable decoder tag name when available.
    pub name: Option<String>,
    /// Whether the tag was explicitly specified.
    pub specified: bool,
}

impl VideoColorTag {
    /// Compact diagnostic representation.
    pub fn summary(&self) -> String {
        let name = self.name.as_deref().unwrap_or("unspecified");
        format!("{name}(code={},specified={})", self.code, self.specified)
    }
}

/// Raw container/codec color metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorMetadata {
    /// Color primaries tag.
    pub primaries: VideoColorTag,
    /// Transfer characteristic tag.
    pub transfer: VideoColorTag,
    /// Matrix coefficients tag.
    pub matrix: VideoColorTag,
}

impl VideoColorMetadata {
    /// Resolve a diagnostic CICP candidate from closed CICP and sampling facts.
    ///
    /// Primaries and transfer must both be explicit and map uniquely. A proven
    /// RGB source needs no YCbCr matrix; every other source must carry an
    /// explicit matrix supported by the decoder conversion contract.
    ///
    /// This is not final pixel authority because an acquisition declaration in
    /// the same probe may take precedence or may be ambiguous. Execution must
    /// use [`DetectedColorInterpretation::executable_color_space_from_probe`].
    pub fn exact_cicp_candidate_for_sampling(
        &self,
        sampling: Option<ProvenVideoSampling>,
    ) -> Option<ColorSpace> {
        self.has_closed_cicp_representation()
            .then(|| exact_cicp_color_space(self, sampling))
            .flatten()
    }

    /// Whether every raw CICP tag has a closed numeric identity whose name and
    /// `specified` bit exactly match the canonical FFmpeg representation.
    ///
    /// This prevents a persisted display name from overriding the standardized
    /// numeric code after deserialization. Unsupported but standardized tags
    /// remain valid diagnostics; they simply cannot produce a color candidate.
    pub fn has_closed_cicp_representation(&self) -> bool {
        cicp_tag_has_canonical_form(&self.primaries, CicpTagKind::Primaries)
            && cicp_tag_has_canonical_form(&self.transfer, CicpTagKind::Transfer)
            && cicp_tag_has_canonical_form(&self.matrix, CicpTagKind::Matrix)
    }

    /// Compact diagnostic representation.
    pub fn summary(&self) -> String {
        format!(
            "primaries={},transfer={},matrix={}",
            self.primaries.summary(),
            self.transfer.summary(),
            self.matrix.summary()
        )
    }
}

/// Scope where an acquisition/color metadata hint was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorMetadataHintScope {
    /// Container-level metadata.
    Container,
    /// Stream-level metadata.
    Stream,
    /// Complete pair inferred from the file name.
    FileName,
}

/// Authority carried by one parsed color metadata field.
///
/// This is assigned once by the media probe from a closed set of supported
/// container/stream declaration fields. Execution code consumes this typed
/// provenance and never reinterprets free-form keys or confidence scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorMetadataHintAuthority {
    /// Human-readable evidence that may be shown as an interpretation suggestion.
    DiagnosticSuggestion,
    /// A supported stream/container field explicitly declaring source identity.
    SourceDeclaration(VideoColorMetadataDeclaration),
}

/// Closed declaration field understood by the media probe.
///
/// Adding support for another vendor/container field requires an explicit
/// variant and probe mapping; arbitrary descriptive keys cannot become source
/// authority by sharing a suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorMetadataDeclaration {
    /// Generic camera-profile field.
    CameraProfile,
    /// Generic camera-log field.
    CameraLog,
    /// Generic camera color-space field.
    CameraColorSpace,
    /// Generic color-profile field.
    ColorProfile,
    /// Generic color-space field.
    ColorSpace,
    /// Combined gamma/gamut field.
    GammaGamut,
    /// Explicit input color-space field.
    InputColorSpace,
    /// Explicit log-profile field.
    LogProfile,
    /// Explicit source color-space field.
    SourceColorSpace,
    /// Sony namespaced color-profile field.
    SonyColorProfile,
    /// Apple Pro Apps namespaced camera-log field.
    AppleProAppsCameraLog,
    /// OpenImageIO color-space field.
    OiioColorSpace,
    /// OpenColorIO color-space field.
    OcioColorSpace,
}

impl VideoColorMetadataDeclaration {
    /// Whether this declaration kind is admitted as source identity authority.
    ///
    /// Generic declaration labels, including `SourceColorSpace`, remain
    /// diagnostic. User authority travels through the separate author Override
    /// contract, never through probe evidence.
    pub const fn is_executable(self) -> bool {
        matches!(self, Self::SonyColorProfile | Self::AppleProAppsCameraLog)
    }

    /// Validate the exact namespaced metadata key represented by this variant.
    pub fn matches_metadata_key(self, key: &str) -> bool {
        match self {
            Self::SonyColorProfile => key.eq_ignore_ascii_case("com.sony.colorProfile"),
            Self::AppleProAppsCameraLog => key.eq_ignore_ascii_case("com.apple.proapps.cameraLog"),
            _ => false,
        }
    }

    /// Re-parse one namespaced declaration into its exact supported identity.
    ///
    /// Execution repeats this closed value check instead of trusting a
    /// serialized authority enum or candidate field. Punctuation and spacing
    /// are insignificant, but extra descriptive text and incomplete
    /// transfer/gamut declarations are not admitted.
    pub fn declared_color_space(self, key: &str, value: &str) -> Option<ColorSpace> {
        if !self.matches_metadata_key(key) {
            return None;
        }
        let value = normalize_color_declaration(value);
        match (self, value.as_str()) {
            (Self::SonyColorProfile, "slog2sgamut" | "sgamutslog2") => {
                Some(ColorSpace::SonySLog2SGamut)
            }
            (Self::SonyColorProfile, "slog3sgamut3" | "sgamut3slog3") => {
                Some(ColorSpace::SonySLog3SGamut3)
            }
            (Self::SonyColorProfile, "slog3sgamut3cine" | "sgamut3cineslog3") => {
                Some(ColorSpace::SonySLog3SGamut3Cine)
            }
            (Self::AppleProAppsCameraLog, "applelog") => Some(ColorSpace::AppleLogBt2020),
            _ => None,
        }
    }
}

fn normalize_color_declaration(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Metadata hint identifying an acquisition or camera-log color space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorMetadataHint {
    /// Metadata scope.
    pub scope: VideoColorMetadataHintScope,
    /// Original metadata key.
    pub key: String,
    /// Original metadata value.
    pub value: String,
    /// Supported color space identified by the hint.
    pub detected_color_space: ColorSpace,
    /// Closed authority classification assigned by the media probe.
    pub authority: VideoColorMetadataHintAuthority,
}

impl VideoColorMetadataHint {
    /// Whether this hint is an explicit, executable source declaration.
    ///
    /// File names and descriptive comments remain useful diagnostics, but they
    /// can never authorize a pixel transform. Only a complete identity carried
    /// by a recognized stream/container declaration key crosses that seam.
    pub fn is_executable_declaration(&self) -> bool {
        match (self.scope, self.authority) {
            (
                VideoColorMetadataHintScope::Container | VideoColorMetadataHintScope::Stream,
                VideoColorMetadataHintAuthority::SourceDeclaration(declaration),
            ) => {
                declaration.is_executable()
                    && declaration.declared_color_space(&self.key, &self.value)
                        == Some(self.detected_color_space)
            }
            _ => false,
        }
    }

    /// Compact diagnostic representation.
    pub fn summary(&self) -> String {
        format!(
            "{:?}:{:?}:{}={}->{:?}",
            self.scope, self.authority, self.key, self.value, self.detected_color_space
        )
    }
}

/// Result of resolving the highest-authority raw acquisition declarations.
///
/// This is diagnostic probe interpretation, not final pixel authority. The
/// final decision additionally binds the exact derived evidence, raw CICP,
/// proven sampling, and all raw hints through
/// [`DetectedColorInterpretation::executable_color_space_from_probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoColorMetadataDeclarationResolution {
    /// No executable acquisition declaration is present.
    Absent,
    /// The highest-priority scope has exactly one declared identity.
    Unique {
        /// Winning raw metadata scope.
        scope: VideoColorMetadataHintScope,
        /// Unambiguous declared source identity.
        color_space: ColorSpace,
    },
    /// The highest-priority scope contains different declared identities.
    Conflicting {
        /// Scope whose declarations conflict.
        scope: VideoColorMetadataHintScope,
    },
}

/// Resolve raw acquisition declarations without using enumeration order as
/// authority.
///
/// Stream declarations explicitly outrank container declarations. Multiple
/// declarations at the winning scope may agree on one identity; different
/// identities at that same scope are ambiguous and fail closed.
pub fn resolve_video_color_metadata_declarations(
    hints: &[VideoColorMetadataHint],
) -> VideoColorMetadataDeclarationResolution {
    let winning_scope = hints
        .iter()
        .filter(|hint| hint.is_executable_declaration())
        .map(|hint| hint.scope)
        .min_by_key(|scope| video_color_metadata_scope_priority(*scope));
    let Some(winning_scope) = winning_scope else {
        return VideoColorMetadataDeclarationResolution::Absent;
    };
    let mut identities = hints
        .iter()
        .filter(|hint| hint.scope == winning_scope && hint.is_executable_declaration())
        .map(|hint| hint.detected_color_space);
    let Some(color_space) = identities.next() else {
        return VideoColorMetadataDeclarationResolution::Absent;
    };
    if identities.any(|identity| identity != color_space) {
        return VideoColorMetadataDeclarationResolution::Conflicting { scope: winning_scope };
    }
    VideoColorMetadataDeclarationResolution::Unique { scope: winning_scope, color_space }
}

fn video_color_metadata_scope_priority(scope: VideoColorMetadataHintScope) -> u8 {
    match scope {
        VideoColorMetadataHintScope::Stream => 0,
        VideoColorMetadataHintScope::Container => 1,
        VideoColorMetadataHintScope::FileName => 2,
    }
}

/// Evidence contributing to an automatic color interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorInterpretationEvidence {
    /// An acquisition metadata hint identified the input.
    MetadataHint {
        /// Metadata scope.
        scope: VideoColorMetadataHintScope,
        /// Original key.
        key: String,
        /// Original value.
        value: String,
        /// Identified color space.
        detected_color_space: ColorSpace,
        /// Closed probe-time authority classification.
        authority: VideoColorMetadataHintAuthority,
    },
    /// Complete CICP tags identified the input.
    ExactCicpTags {
        /// Color primaries.
        primaries: VideoColorTag,
        /// Transfer characteristic.
        transfer: VideoColorTag,
        /// Matrix coefficients.
        matrix: VideoColorTag,
        /// Identified color space.
        detected_color_space: ColorSpace,
    },
    /// Compatible partial CICP tags suggested an input candidate.
    PartialCicpTags {
        /// Color primaries.
        primaries: VideoColorTag,
        /// Transfer characteristic.
        transfer: VideoColorTag,
        /// Matrix coefficients.
        matrix: VideoColorTag,
        /// Identified color space.
        detected_color_space: ColorSpace,
    },
    /// Present CICP tags did not map to a supported input.
    UnsupportedCicpTags {
        /// Color primaries.
        primaries: VideoColorTag,
        /// Transfer characteristic.
        transfer: VideoColorTag,
        /// Matrix coefficients.
        matrix: VideoColorTag,
    },
    /// Decoder-side metadata was unavailable.
    DecoderUnavailable,
    /// An embedded ICC profile was inspected.
    ///
    /// The current mapping is diagnostic because it is based on a profile
    /// description rather than verified chromaticity/TRC data or an ICC
    /// processor route.
    IccProfile {
        /// Supported mapped identity, when available.
        mapped_color_space: Option<ColorSpace>,
        /// Embedded profile name, when available.
        profile_name: Option<String>,
    },
}

/// Non-fatal ambiguity retained with a color interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorInterpretationWarning {
    /// Multiple competing metadata hints were retained.
    MultipleMetadataHints {
        /// Selected hint.
        selected: VideoColorMetadataHint,
        /// Lower-priority hints.
        ignored: Vec<VideoColorMetadataHint>,
    },
    /// An explicit hint overrode conflicting CICP evidence.
    MetadataHintOverridesCicpTags {
        /// Selected hint.
        selected: VideoColorMetadataHint,
        /// Conflicting CICP identity.
        cicp_color_space: ColorSpace,
        /// Raw conflicting metadata.
        cicp_metadata: VideoColorMetadata,
    },
    /// Free-form descriptive metadata supplied weak fallback evidence.
    DescriptiveMetadataHintInference {
        /// Selected weak hint.
        selected: VideoColorMetadataHint,
    },
    /// Stronger evidence overrode descriptive hints.
    LowerPriorityMetadataHints {
        /// Selected detection method.
        selected_method: VideoColorDetectionMethod,
        /// Selected color space.
        selected_color_space: ColorSpace,
        /// Retained lower-priority hints.
        ignored: Vec<VideoColorMetadataHint>,
    },
    /// Only partial CICP evidence was available.
    PartialCicpTags {
        /// Inferred color space.
        detected_color_space: ColorSpace,
    },
    /// No CICP tags were present.
    MissingCicpTags,
    /// CICP tags were present but unsupported.
    UnsupportedCicpTags,
    /// Decoder metadata was unavailable.
    DecoderUnavailable,
    /// ICC profile parsed but did not map to a supported identity.
    IccProfileUnmapped {
        /// Embedded profile name, when available.
        profile_name: Option<String>,
        /// Mapping failure reason.
        reason: String,
    },
    /// ICC and CICP evidence disagreed.
    IccCicpMismatch {
        /// ICC-derived identity.
        icc_color_space: ColorSpace,
        /// CICP-derived identity.
        cicp_color_space: ColorSpace,
    },
}

impl VideoColorInterpretationWarning {
    /// Compact diagnostic representation.
    pub fn summary(&self) -> String {
        match self {
            Self::MultipleMetadataHints { selected, ignored } => format!(
                "multiple_hints(selected={},ignored={})",
                selected.summary(),
                ignored.iter().map(VideoColorMetadataHint::summary).collect::<Vec<_>>().join("|")
            ),
            Self::MetadataHintOverridesCicpTags {
                selected,
                cicp_color_space,
                cicp_metadata,
            } => format!(
                "hint_overrides_cicp(selected={},cicp={cicp_color_space:?},metadata={})",
                selected.summary(),
                cicp_metadata.summary()
            ),
            Self::DescriptiveMetadataHintInference { selected } => {
                format!("descriptive_hint_inference(selected={})", selected.summary())
            }
            Self::LowerPriorityMetadataHints {
                selected_method,
                selected_color_space,
                ignored,
            } => format!(
                "lower_priority_hints(selected_method={selected_method:?},selected={selected_color_space:?},ignored={})",
                ignored.iter().map(VideoColorMetadataHint::summary).collect::<Vec<_>>().join("|")
            ),
            Self::PartialCicpTags { detected_color_space } => {
                format!("partial_cicp(detected={detected_color_space:?})")
            }
            Self::MissingCicpTags => "missing_cicp".to_owned(),
            Self::UnsupportedCicpTags => "unsupported_cicp".to_owned(),
            Self::DecoderUnavailable => "decoder_unavailable".to_owned(),
            Self::IccProfileUnmapped { profile_name, reason } => {
                format!("icc_profile_unmapped(profile={profile_name:?},reason={reason})")
            }
            Self::IccCicpMismatch { icc_color_space, cicp_color_space } => {
                format!("icc_cicp_mismatch(icc={icc_color_space:?},cicp={cicp_color_space:?})")
            }
        }
    }
}

/// Structured automatic input-color interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedColorInterpretation {
    /// Supported diagnostic candidate suggested by the available evidence.
    ///
    /// This field is intentionally not execution authority. Call
    /// [`Self::executable_color_space_from_probe`] at every Preview, Export,
    /// proxy, or author-description seam.
    pub candidate_color_space: Option<ColorSpace>,
    /// Confidence in the selected identity.
    pub confidence: VideoColorInterpretationConfidence,
    /// Evidence source class.
    pub source: VideoColorSpaceSource,
    /// Selection method.
    pub method: VideoColorDetectionMethod,
    /// Structured evidence records.
    #[serde(default)]
    pub evidence: Vec<VideoColorInterpretationEvidence>,
    /// Non-fatal ambiguity records.
    #[serde(default)]
    pub warnings: Vec<VideoColorInterpretationWarning>,
    /// Whether explicit author interpretation may replace this result.
    pub user_overridable: bool,
}

impl DetectedColorInterpretation {
    /// Return the sole metadata-derived source identity allowed to drive pixels.
    ///
    /// The decision replays the closed probe rules from complete raw facts. A
    /// serialized confidence, candidate, authority enum, display name, or
    /// derived evidence record cannot independently authorize execution.
    /// File-name, descriptive-text, partial-CICP, and profile-name suggestions
    /// therefore remain non-executable even if their confidence changes.
    pub fn executable_color_space_from_probe(
        &self,
        sampling: Option<ProvenVideoSampling>,
        metadata: Option<&VideoColorMetadata>,
        metadata_hints: &[VideoColorMetadataHint],
    ) -> Option<ColorSpace> {
        let candidate = self.candidate_color_space?;
        let sampling = sampling?;
        let metadata = metadata?;
        if self.source != VideoColorSpaceSource::Metadata {
            return None;
        }
        if !metadata.has_closed_cicp_representation() {
            return None;
        }

        let provenance_matches = match self.method {
            VideoColorDetectionMethod::MetadataHint => {
                let VideoColorMetadataDeclarationResolution::Unique { scope, color_space } =
                    resolve_video_color_metadata_declarations(metadata_hints)
                else {
                    return None;
                };
                color_space == candidate
                    && sampling_matrix_is_executable(sampling, &metadata.matrix)
                    && metadata_declaration_evidence_matches_raw(
                        &self.evidence,
                        metadata_hints,
                        scope,
                        candidate,
                    )
            }
            VideoColorDetectionMethod::CicpTags => {
                if resolve_video_color_metadata_declarations(metadata_hints)
                    != VideoColorMetadataDeclarationResolution::Absent
                {
                    return None;
                }
                metadata.exact_cicp_candidate_for_sampling(Some(sampling)) == Some(candidate)
                    && exact_cicp_evidence_matches_raw(&self.evidence, metadata, candidate)
            }
            VideoColorDetectionMethod::IccProfile
            | VideoColorDetectionMethod::MissingMetadata
            | VideoColorDetectionMethod::UnsupportedCicpTags
            | VideoColorDetectionMethod::DecoderUnavailable => false,
        };
        provenance_matches.then_some(candidate)
    }

    /// Explicit fail-closed evidence when a decoder could not be opened.
    pub fn decoder_unavailable() -> Self {
        Self {
            candidate_color_space: None,
            confidence: VideoColorInterpretationConfidence::None,
            source: VideoColorSpaceSource::DecoderUnavailable,
            method: VideoColorDetectionMethod::DecoderUnavailable,
            evidence: vec![VideoColorInterpretationEvidence::DecoderUnavailable],
            warnings: vec![VideoColorInterpretationWarning::DecoderUnavailable],
            user_overridable: true,
        }
    }
}

fn metadata_declaration_evidence_matches_raw(
    evidence: &[VideoColorInterpretationEvidence],
    metadata_hints: &[VideoColorMetadataHint],
    selected_scope: VideoColorMetadataHintScope,
    selected_color_space: ColorSpace,
) -> bool {
    let mut selected_evidence_found = false;
    for evidence in evidence {
        let VideoColorInterpretationEvidence::MetadataHint {
            scope,
            key,
            value,
            detected_color_space,
            authority: VideoColorMetadataHintAuthority::SourceDeclaration(declaration),
        } = evidence
        else {
            continue;
        };
        let raw_match = metadata_hints.iter().any(|hint| {
            hint.scope == *scope
                && hint.key == *key
                && hint.value == *value
                && hint.detected_color_space == *detected_color_space
                && hint.authority
                    == VideoColorMetadataHintAuthority::SourceDeclaration(*declaration)
                && hint.is_executable_declaration()
        });
        if !raw_match {
            return false;
        }
        if *scope == selected_scope && *detected_color_space == selected_color_space {
            selected_evidence_found = true;
        }
    }
    selected_evidence_found
}

fn exact_cicp_evidence_matches_raw(
    evidence: &[VideoColorInterpretationEvidence],
    metadata: &VideoColorMetadata,
    candidate: ColorSpace,
) -> bool {
    let mut exact_evidence_found = false;
    for evidence in evidence {
        match evidence {
            VideoColorInterpretationEvidence::ExactCicpTags {
                primaries,
                transfer,
                matrix,
                detected_color_space,
            } => {
                if primaries != &metadata.primaries
                    || transfer != &metadata.transfer
                    || matrix != &metadata.matrix
                    || *detected_color_space != candidate
                {
                    return false;
                }
                exact_evidence_found = true;
            }
            VideoColorInterpretationEvidence::PartialCicpTags { .. }
            | VideoColorInterpretationEvidence::UnsupportedCicpTags { .. } => return false,
            VideoColorInterpretationEvidence::MetadataHint { .. }
            | VideoColorInterpretationEvidence::DecoderUnavailable
            | VideoColorInterpretationEvidence::IccProfile { .. } => {}
        }
    }
    exact_evidence_found
}

fn exact_cicp_color_space(
    metadata: &VideoColorMetadata,
    sampling: Option<ProvenVideoSampling>,
) -> Option<ColorSpace> {
    let primaries = canonical_cicp_tag_name(&metadata.primaries, CicpTagKind::Primaries)?;
    let transfer = canonical_cicp_tag_name(&metadata.transfer, CicpTagKind::Transfer)?;
    let color_space = ColorSpace::from_ffmpeg_colorimetry(primaries, transfer)?;
    let sampling = sampling?;
    if !sampling_matrix_is_executable(sampling, &metadata.matrix) {
        return None;
    }
    Some(color_space)
}

fn sampling_matrix_is_executable(sampling: ProvenVideoSampling, matrix: &VideoColorTag) -> bool {
    if sampling.pixel_format.is_rgb() {
        return true;
    }
    canonical_cicp_tag_name(matrix, CicpTagKind::Matrix).is_some_and(is_supported_yuv_matrix)
}

fn is_supported_yuv_matrix(matrix: &str) -> bool {
    matches!(
        matrix.to_ascii_lowercase().as_str(),
        "bt709" | "fcc" | "bt470bg" | "smpte170m" | "smpte240m" | "bt2020nc" | "bt2020ncl"
    )
}

#[derive(Debug, Clone, Copy)]
enum CicpTagKind {
    Primaries,
    Transfer,
    Matrix,
}

fn cicp_tag_has_canonical_form(tag: &VideoColorTag, kind: CicpTagKind) -> bool {
    let Some(canonical_name) = canonical_cicp_name(kind, tag.code) else {
        return false;
    };
    match canonical_name {
        Some(canonical_name) => tag.specified && tag.name.as_deref() == Some(canonical_name),
        None => !tag.specified && tag.name.is_none(),
    }
}

fn canonical_cicp_tag_name(tag: &VideoColorTag, kind: CicpTagKind) -> Option<&'static str> {
    if !cicp_tag_has_canonical_form(tag, kind) {
        return None;
    }
    canonical_cicp_name(kind, tag.code).flatten()
}

fn canonical_cicp_name(kind: CicpTagKind, code: i32) -> Option<Option<&'static str>> {
    let name = match (kind, code) {
        (CicpTagKind::Primaries, 1) => Some("bt709"),
        (CicpTagKind::Primaries, 2) => None,
        (CicpTagKind::Primaries, 4) => Some("bt470m"),
        (CicpTagKind::Primaries, 5) => Some("bt470bg"),
        (CicpTagKind::Primaries, 6) => Some("smpte170m"),
        (CicpTagKind::Primaries, 7) => Some("smpte240m"),
        (CicpTagKind::Primaries, 8) => Some("film"),
        (CicpTagKind::Primaries, 9) => Some("bt2020"),
        (CicpTagKind::Primaries, 10) => Some("smpte428"),
        (CicpTagKind::Primaries, 11) => Some("smpte431"),
        (CicpTagKind::Primaries, 12) => Some("smpte432"),
        (CicpTagKind::Primaries, 22) => Some("jedec-p22"),
        (CicpTagKind::Transfer, 1) => Some("bt709"),
        (CicpTagKind::Transfer, 2) => None,
        (CicpTagKind::Transfer, 4) => Some("bt470m"),
        (CicpTagKind::Transfer, 5) => Some("bt470bg"),
        (CicpTagKind::Transfer, 6) => Some("smpte170m"),
        (CicpTagKind::Transfer, 7) => Some("smpte240m"),
        (CicpTagKind::Transfer, 8) => Some("linear"),
        (CicpTagKind::Transfer, 9) => Some("log100"),
        (CicpTagKind::Transfer, 10) => Some("log316"),
        (CicpTagKind::Transfer, 11) => Some("iec61966-2-4"),
        (CicpTagKind::Transfer, 12) => Some("bt1361e"),
        (CicpTagKind::Transfer, 13) => Some("iec61966-2-1"),
        (CicpTagKind::Transfer, 14) => Some("bt2020-10"),
        (CicpTagKind::Transfer, 15) => Some("bt2020-12"),
        (CicpTagKind::Transfer, 16) => Some("smpte2084"),
        (CicpTagKind::Transfer, 17) => Some("smpte428"),
        (CicpTagKind::Transfer, 18) => Some("arib-std-b67"),
        (CicpTagKind::Matrix, 0) => Some("gbr"),
        (CicpTagKind::Matrix, 1) => Some("bt709"),
        (CicpTagKind::Matrix, 2) => None,
        (CicpTagKind::Matrix, 4) => Some("fcc"),
        (CicpTagKind::Matrix, 5) => Some("bt470bg"),
        (CicpTagKind::Matrix, 6) => Some("smpte170m"),
        (CicpTagKind::Matrix, 7) => Some("smpte240m"),
        (CicpTagKind::Matrix, 8) => Some("ycgco"),
        (CicpTagKind::Matrix, 9) => Some("bt2020nc"),
        (CicpTagKind::Matrix, 10) => Some("bt2020c"),
        (CicpTagKind::Matrix, 11) => Some("smpte2085"),
        (CicpTagKind::Matrix, 12) => Some("chroma-derived-nc"),
        (CicpTagKind::Matrix, 13) => Some("chroma-derived-c"),
        (CicpTagKind::Matrix, 14) => Some("ictcp"),
        _ => return None,
    };
    Some(name)
}

/// HDR-related side-data kind detected during probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoHdrSideDataKind {
    /// SMPTE ST 2086 mastering-display metadata.
    MasteringDisplayMetadata,
    /// MaxCLL/MaxFALL content-light metadata.
    ContentLightLevel,
    /// HDR10+ dynamic metadata.
    DynamicHdr10Plus,
    /// Dolby Vision configuration metadata.
    DolbyVisionConfig,
    /// Embedded ICC profile.
    IccProfile,
}

/// Summary of HDR-related side data on a video stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoHdrMetadataSummary {
    /// Semantic side-data kind.
    pub kind: VideoHdrSideDataKind,
    /// Raw payload size.
    pub payload_size: usize,
    /// Parsed stable payload, when supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<VideoHdrMetadataPayload>,
}

impl VideoHdrMetadataSummary {
    /// Compact diagnostic representation.
    pub fn summary(&self) -> String {
        let payload = self
            .payload
            .as_ref()
            .map(VideoHdrMetadataPayload::summary)
            .unwrap_or_else(|| "unparsed".to_owned());
        format!(
            "{:?}(bytes={},payload={payload})",
            self.kind, self.payload_size
        )
    }
}

/// Native decoder-probed audio channel layout.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChannelLayout {
    /// Decoder proved one complete semantic signal layout and canonical order.
    Exact(AudioChannelLayout),
    /// Decoder supplied a count without speaker semantics.
    Unspecified(u8),
    /// Decoder supplied a named or ordered layout that this contract cannot represent.
    Unsupported(u8),
}

impl ChannelLayout {
    /// Exact layout-independent mono probe fact.
    #[allow(non_upper_case_globals)]
    pub const Mono: Self = Self::Exact(AudioChannelLayout::Mono);
    /// Exact front-left/front-right probe fact.
    #[allow(non_upper_case_globals)]
    pub const Stereo: Self = Self::Exact(AudioChannelLayout::Stereo);
    /// Exact 5.1(side) probe fact.
    #[allow(non_upper_case_globals)]
    pub const Surround51Side: Self = Self::Exact(AudioChannelLayout::Surround51Side);
    /// Exact 5.1(back) probe fact.
    #[allow(non_upper_case_globals)]
    pub const Surround51Back: Self = Self::Exact(AudioChannelLayout::Surround51Back);
    /// Exact 7.1 probe fact.
    #[allow(non_upper_case_globals)]
    pub const Surround71: Self = Self::Exact(AudioChannelLayout::Surround71);

    /// Channel extent reported for this native layout.
    pub const fn channel_count(&self) -> u8 {
        match self {
            Self::Exact(layout) => layout.channel_count_u8(),
            Self::Unspecified(channels) | Self::Unsupported(channels) => *channels,
        }
    }

    /// Preserve probe semantics in the shared signal-layout value.
    pub fn exact_signal_layout(&self) -> Option<AudioChannelLayout> {
        match self {
            Self::Exact(layout) => Some(*layout),
            Self::Unspecified(channels) if *channels > 0 && *channels <= MAX_AUDIO_CHANNELS => {
                AudioChannelLayout::discrete(*channels).ok()
            }
            Self::Unspecified(_) | Self::Unsupported(_) => None,
        }
    }
}

/// Persistable video-stream probe facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    /// Absolute container stream index.
    pub index: u32,
    /// Encoded codec.
    pub codec: VideoCodec,
    /// Stream-local declared duration.
    #[serde(default)]
    pub duration: Option<Duration>,
    /// Decoder-proven codec profile.
    #[serde(default)]
    pub codec_profile: VideoCodecProfile,
    /// Encoded raster width.
    pub width: u32,
    /// Encoded raster height.
    pub height: u32,
    /// Exact scan, sample geometry, and source display-orientation evidence.
    #[serde(default)]
    pub picture: PictureStreamMetadata,
    /// Canonicalized average frame rate.
    pub frame_rate: Rational,
    /// Whether the frame rate came from positive decoder evidence.
    #[serde(default)]
    pub frame_rate_proven: bool,
    /// Encoded pixel format or a non-authoritative storage fallback.
    ///
    /// This field is executable evidence only through
    /// [`Self::proven_sampling`].
    pub pixel_format: PixelFormat,
    /// Whether the pixel format was explicitly proven by the probe Adapter.
    #[serde(default)]
    pub pixel_format_proven: bool,
    /// Encoded quantization range.
    #[serde(default)]
    pub color_range: DecodedVideoRange,
    /// Full structured interpretation and the sole source-color authority.
    pub color_interpretation: DetectedColorInterpretation,
    /// Raw CICP metadata.
    pub color_metadata: Option<VideoColorMetadata>,
    /// Acquisition metadata hints.
    #[serde(default)]
    pub color_metadata_hints: Vec<VideoColorMetadataHint>,
    /// HDR/ICC side-data summaries.
    #[serde(default)]
    pub hdr_metadata: Vec<VideoHdrMetadataSummary>,
    /// Nominal encoded component bit depth or a non-authoritative storage
    /// fallback. This field is executable evidence only through
    /// [`Self::proven_sampling`].
    pub bit_depth: u8,
    /// Whether Alpha is encoded, or a non-authoritative storage fallback. This
    /// field is executable evidence only through [`Self::proven_sampling`].
    pub has_alpha: bool,
    /// Average encoded bitrate in bits per second.
    pub avg_bitrate: u64,
    /// Exact or bounded-probe-proven frame count.
    pub total_frames: Option<u64>,
}

impl VideoStreamInfo {
    /// Return the only source identity allowed to drive media pixels.
    pub fn executable_color_space(&self) -> Option<ColorSpace> {
        self.color_interpretation.executable_color_space_from_probe(
            self.proven_sampling(),
            self.color_metadata.as_ref(),
            &self.color_metadata_hints,
        )
    }

    /// Return exact sampling evidence only when every persisted fact is proven
    /// and internally consistent.
    ///
    /// An unmapped decoder format is deliberately `None`; its serialized
    /// fallback must never authorize native-surface selection, proxy precision,
    /// or destructive Alpha reduction.
    pub fn proven_sampling(&self) -> Option<ProvenVideoSampling> {
        if !self.pixel_format_proven {
            return None;
        }
        let sampling = ProvenVideoSampling {
            pixel_format: self.pixel_format,
            bit_depth: self.bit_depth,
            has_alpha: self.has_alpha,
        };
        (sampling.bit_depth == sampling.pixel_format.bit_depth()
            && sampling.has_alpha == sampling.pixel_format.has_alpha())
        .then_some(sampling)
    }
}

/// Persistable audio-stream probe facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    /// Absolute container stream index.
    pub index: u32,
    /// Container stream identifier, when present.
    pub stream_id: Option<i32>,
    /// Normalized language metadata.
    pub language: Option<String>,
    /// Human-readable stream title.
    pub title: Option<String>,
    /// Whether the container marks this stream as default.
    pub is_default: bool,
    /// Encoded codec.
    pub codec: AudioCodec,
    /// Stream-local declared duration.
    #[serde(default)]
    pub duration: Option<Duration>,
    /// Native sample rate.
    pub sample_rate: u32,
    /// Reported channel extent.
    pub channels: u8,
    /// Native semantic channel layout evidence.
    pub channel_layout: ChannelLayout,
    /// Nominal encoded sample bit depth.
    pub bit_depth: u16,
    /// Average encoded bitrate in bits per second.
    pub avg_bitrate: u64,
}

/// One explicitly selected physical audio stream and its source revision.
///
/// This is a short-lived Adapter value, not author state. Asset Component
/// catalogs produce it only after their stable logical component identity
/// matches current probe evidence.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioSourceSelection {
    stream_index: u32,
    source_layout: ChannelLayout,
    source_fingerprint: MediaFileFingerprint,
}

impl AudioSourceSelection {
    /// Create a physical selection from explicit probe or fixture evidence.
    pub const fn new(
        stream_index: u32,
        source_layout: ChannelLayout,
        source_fingerprint: MediaFileFingerprint,
    ) -> Self {
        Self { stream_index, source_layout, source_fingerprint }
    }

    /// Capture an execution selection from one already-validated stream probe.
    pub fn from_stream(stream: &AudioStreamInfo, source_fingerprint: MediaFileFingerprint) -> Self {
        Self::new(
            stream.index,
            stream.channel_layout.clone(),
            source_fingerprint,
        )
    }

    /// Absolute container stream index used by the media Adapter.
    pub const fn stream_index(&self) -> u32 {
        self.stream_index
    }

    /// Exact native semantic layout observed during probing.
    pub const fn source_layout(&self) -> &ChannelLayout {
        &self.source_layout
    }

    /// File revision whose probe evidence authorized this selection.
    pub const fn source_fingerprint(&self) -> MediaFileFingerprint {
        self.source_fingerprint
    }
}

/// Immutable, persistable result of probing one file-backed media revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaProbeSnapshot {
    /// Container duration.
    pub duration: Duration,
    /// File size observed for this probe.
    pub file_size: u64,
    /// Decoder/container format name.
    pub container: String,
    /// Video streams in container order.
    pub video_streams: Vec<VideoStreamInfo>,
    /// Audio streams in container order.
    pub audio_streams: Vec<AudioStreamInfo>,
    /// Whether at least one video stream exists.
    pub has_video: bool,
    /// Whether at least one audio stream exists.
    pub has_audio: bool,
}

impl MediaProbeSnapshot {
    /// Primary video stream in probe order.
    pub fn primary_video(&self) -> Option<&VideoStreamInfo> {
        self.video_streams.first()
    }

    /// Primary audio stream in probe order.
    pub fn primary_audio(&self) -> Option<&AudioStreamInfo> {
        self.audio_streams.first()
    }

    /// Estimate container frame count from duration and the primary stream rate.
    pub fn estimated_frames(&self) -> Option<u64> {
        let video = self.primary_video()?;
        Some((self.duration.as_secs_f64() * video.frame_rate.to_f64()).ceil() as u64)
    }
}

/// Concise shared name for the canonical persisted probe snapshot.
pub type MediaInfo = MediaProbeSnapshot;

/// Whether a path uses a supported picture-file extension.
///
/// This is only a bounded-probe admission hint and never proves one frame.
pub fn is_picture_file_extension(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "bmp"
            | "dpx"
            | "exr"
            | "gif"
            | "heic"
            | "heif"
            | "jpeg"
            | "jpg"
            | "jxl"
            | "png"
            | "tga"
            | "tif"
            | "tiff"
            | "webp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, thread};

    fn video_stream(pixel_format_proven: bool, bit_depth: u8, has_alpha: bool) -> VideoStreamInfo {
        VideoStreamInfo {
            index: 0,
            codec: VideoCodec::H265,
            duration: Some(Duration::from_secs(1)),
            codec_profile: VideoCodecProfile::HevcMain10,
            width: 3840,
            height: 2160,
            picture: PictureStreamMetadata::default(),
            frame_rate: Rational::new(25, 1),
            frame_rate_proven: true,
            pixel_format: PixelFormat::P010,
            pixel_format_proven,
            color_range: DecodedVideoRange::Limited,
            color_interpretation: DetectedColorInterpretation::decoder_unavailable(),
            color_metadata: None,
            color_metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
            bit_depth,
            has_alpha,
            avg_bitrate: 20_000_000,
            total_frames: Some(25),
        }
    }

    fn declared_color_hint() -> VideoColorMetadataHint {
        VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "com.sony.colorProfile".to_owned(),
            value: "S-Log3 / S-Gamut3.Cine".to_owned(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            authority: VideoColorMetadataHintAuthority::SourceDeclaration(
                VideoColorMetadataDeclaration::SonyColorProfile,
            ),
        }
    }

    fn declared_color_interpretation() -> DetectedColorInterpretation {
        let hint = declared_color_hint();
        DetectedColorInterpretation {
            candidate_color_space: Some(ColorSpace::SonySLog3SGamut3Cine),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
            evidence: vec![VideoColorInterpretationEvidence::MetadataHint {
                scope: hint.scope,
                key: hint.key,
                value: hint.value,
                detected_color_space: hint.detected_color_space,
                authority: hint.authority,
            }],
            warnings: Vec::new(),
            user_overridable: true,
        }
    }

    fn color_metadata_with_matrix(matrix: VideoColorTag) -> VideoColorMetadata {
        VideoColorMetadata {
            primaries: VideoColorTag { code: 2, name: None, specified: false },
            transfer: VideoColorTag { code: 2, name: None, specified: false },
            matrix,
        }
    }

    fn rgb_sampling() -> ProvenVideoSampling {
        ProvenVideoSampling {
            pixel_format: PixelFormat::Rgb24,
            bit_depth: 8,
            has_alpha: false,
        }
    }

    fn interpretation_executable(
        interpretation: &DetectedColorInterpretation,
        sampling: Option<ProvenVideoSampling>,
        metadata: Option<&VideoColorMetadata>,
        metadata_hints: &[VideoColorMetadataHint],
    ) -> Option<ColorSpace> {
        interpretation.executable_color_space_from_probe(sampling, metadata, metadata_hints)
    }

    #[test]
    fn proven_sampling_requires_proof_and_internal_consistency() {
        assert_eq!(
            video_stream(true, 10, false).proven_sampling(),
            Some(ProvenVideoSampling {
                pixel_format: PixelFormat::P010,
                bit_depth: 10,
                has_alpha: false,
            })
        );
        assert_eq!(video_stream(false, 8, false).proven_sampling(), None);
        assert_eq!(video_stream(true, 8, false).proven_sampling(), None);
        assert_eq!(video_stream(true, 10, true).proven_sampling(), None);
    }

    #[test]
    fn declaration_requires_matrix_compatible_with_proven_sampling_family() {
        let mut yuv = video_stream(true, 10, false);
        yuv.color_interpretation = declared_color_interpretation();
        yuv.color_metadata_hints = vec![declared_color_hint()];
        yuv.color_metadata = Some(color_metadata_with_matrix(VideoColorTag {
            code: 2,
            name: None,
            specified: false,
        }));
        assert_eq!(yuv.executable_color_space(), None);

        yuv.color_metadata = Some(color_metadata_with_matrix(VideoColorTag {
            code: 0,
            name: Some("gbr".to_owned()),
            specified: true,
        }));
        assert_eq!(
            yuv.executable_color_space(),
            None,
            "RGB matrix metadata cannot authorize YUV sampling"
        );

        yuv.color_metadata = Some(color_metadata_with_matrix(VideoColorTag {
            code: 1,
            name: Some("bt709".to_owned()),
            specified: true,
        }));
        assert_eq!(
            yuv.executable_color_space(),
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );

        let mut rgb = yuv.clone();
        rgb.pixel_format = PixelFormat::Rgb24;
        rgb.bit_depth = 8;
        rgb.color_metadata = Some(color_metadata_with_matrix(VideoColorTag {
            code: 2,
            name: None,
            specified: false,
        }));
        assert_eq!(
            rgb.executable_color_space(),
            Some(ColorSpace::SonySLog3SGamut3Cine),
            "proven RGB sampling has no YCbCr matrix obligation"
        );

        rgb.pixel_format_proven = false;
        rgb.color_metadata = Some(color_metadata_with_matrix(VideoColorTag {
            code: 0,
            name: Some("gbr".to_owned()),
            specified: true,
        }));
        assert_eq!(
            rgb.executable_color_space(),
            None,
            "unknown sampling cannot be authorized by an RGB matrix tag"
        );
    }

    #[test]
    fn forged_or_generic_declaration_key_cannot_authorize_pixels() {
        let sampling = Some(rgb_sampling());
        let metadata =
            color_metadata_with_matrix(VideoColorTag { code: 2, name: None, specified: false });
        let raw_hints = vec![declared_color_hint()];
        let valid = declared_color_interpretation();
        assert_eq!(
            interpretation_executable(&valid, sampling, Some(&metadata), &raw_hints),
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );

        let mut forged = valid.clone();
        let VideoColorInterpretationEvidence::MetadataHint { key, .. } = &mut forged.evidence[0]
        else {
            panic!("declaration evidence")
        };
        *key = "not_a_color_space".to_owned();
        assert_eq!(
            interpretation_executable(&forged, sampling, Some(&metadata), &raw_hints),
            None
        );

        let mut punctuation_forged = declared_color_interpretation();
        let VideoColorInterpretationEvidence::MetadataHint { key, .. } =
            &mut punctuation_forged.evidence[0]
        else {
            panic!("declaration evidence")
        };
        *key = "com_sony_colorProfile".to_owned();
        assert_eq!(
            interpretation_executable(&punctuation_forged, sampling, Some(&metadata), &raw_hints,),
            None,
            "normalization must not turn a look-alike key into a vendor declaration"
        );

        let mut forged_value = declared_color_interpretation();
        let VideoColorInterpretationEvidence::MetadataHint { value, .. } =
            &mut forged_value.evidence[0]
        else {
            panic!("declaration evidence")
        };
        *value = "Apple Log".to_owned();
        assert_eq!(
            interpretation_executable(&forged_value, sampling, Some(&metadata), &raw_hints),
            None,
            "a serialized authority enum cannot replace exact vendor-value parsing"
        );

        let mut wrong_vendor_space = declared_color_interpretation();
        wrong_vendor_space.candidate_color_space = Some(ColorSpace::AppleLogBt2020);
        let VideoColorInterpretationEvidence::MetadataHint { detected_color_space, .. } =
            &mut wrong_vendor_space.evidence[0]
        else {
            panic!("declaration evidence")
        };
        *detected_color_space = ColorSpace::AppleLogBt2020;
        assert_eq!(
            interpretation_executable(&wrong_vendor_space, sampling, Some(&metadata), &raw_hints,),
            None,
            "a Sony declaration cannot authorize an unrelated vendor identity"
        );

        let mut generic = valid;
        let VideoColorInterpretationEvidence::MetadataHint { key, authority, .. } =
            &mut generic.evidence[0]
        else {
            panic!("declaration evidence")
        };
        *key = "source_color_space".to_owned();
        *authority = VideoColorMetadataHintAuthority::SourceDeclaration(
            VideoColorMetadataDeclaration::SourceColorSpace,
        );
        assert_eq!(
            interpretation_executable(&generic, sampling, Some(&metadata), &raw_hints),
            None
        );
    }

    #[test]
    fn diagnostic_hint_cannot_gain_execution_authority_from_confidence_or_serde() {
        let raw_hint = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "not_a_color_space".to_owned(),
            value: "S-Log3 / S-Gamut3.Cine".to_owned(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            authority: VideoColorMetadataHintAuthority::DiagnosticSuggestion,
        };
        let interpretation = DetectedColorInterpretation {
            candidate_color_space: Some(ColorSpace::SonySLog3SGamut3Cine),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
            evidence: vec![VideoColorInterpretationEvidence::MetadataHint {
                scope: VideoColorMetadataHintScope::Stream,
                key: "not_a_color_space".to_owned(),
                value: "S-Log3 / S-Gamut3.Cine".to_owned(),
                detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
                authority: VideoColorMetadataHintAuthority::DiagnosticSuggestion,
            }],
            warnings: Vec::new(),
            user_overridable: true,
        };
        let metadata =
            color_metadata_with_matrix(VideoColorTag { code: 2, name: None, specified: false });

        assert_eq!(
            interpretation_executable(
                &interpretation,
                Some(rgb_sampling()),
                Some(&metadata),
                std::slice::from_ref(&raw_hint),
            ),
            None
        );
        let encoded = serde_json::to_vec(&interpretation).expect("serialize interpretation");
        let decoded: DetectedColorInterpretation =
            serde_json::from_slice(&encoded).expect("deserialize interpretation");
        assert_eq!(
            interpretation_executable(
                &decoded,
                Some(rgb_sampling()),
                Some(&metadata),
                std::slice::from_ref(&raw_hint),
            ),
            None
        );
    }

    #[test]
    fn inconsistent_declaration_provenance_fails_closed() {
        let mut interpretation = DetectedColorInterpretation {
            candidate_color_space: Some(ColorSpace::SonySLog3SGamut3Cine),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
            evidence: vec![VideoColorInterpretationEvidence::MetadataHint {
                scope: VideoColorMetadataHintScope::FileName,
                key: "filename".to_owned(),
                value: "S-Log3_S-Gamut3.Cine.mov".to_owned(),
                detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
                authority: VideoColorMetadataHintAuthority::SourceDeclaration(
                    VideoColorMetadataDeclaration::CameraProfile,
                ),
            }],
            warnings: Vec::new(),
            user_overridable: true,
        };

        let metadata =
            color_metadata_with_matrix(VideoColorTag { code: 2, name: None, specified: false });
        assert_eq!(
            interpretation_executable(&interpretation, Some(rgb_sampling()), Some(&metadata), &[],),
            None
        );
        interpretation.source = VideoColorSpaceSource::UnsupportedMetadata;
        interpretation.evidence[0] = VideoColorInterpretationEvidence::MetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "camera_profile".to_owned(),
            value: "S-Log3 / S-Gamut3.Cine".to_owned(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            authority: VideoColorMetadataHintAuthority::SourceDeclaration(
                VideoColorMetadataDeclaration::CameraProfile,
            ),
        };
        assert_eq!(
            interpretation_executable(&interpretation, Some(rgb_sampling()), Some(&metadata), &[],),
            None
        );
    }

    #[test]
    fn cicp_numeric_code_name_and_specified_bit_must_be_canonical() {
        let sampling = Some(ProvenVideoSampling {
            pixel_format: PixelFormat::Yuv420p,
            bit_depth: 8,
            has_alpha: false,
        });
        let valid = VideoColorMetadata {
            primaries: VideoColorTag {
                code: 1,
                name: Some("bt709".to_owned()),
                specified: true,
            },
            transfer: VideoColorTag {
                code: 1,
                name: Some("bt709".to_owned()),
                specified: true,
            },
            matrix: VideoColorTag {
                code: 1,
                name: Some("bt709".to_owned()),
                specified: true,
            },
        };
        assert_eq!(
            valid.exact_cicp_candidate_for_sampling(sampling),
            Some(ColorSpace::Rec709)
        );

        let pal = VideoColorMetadata {
            primaries: VideoColorTag {
                code: 5,
                name: Some("bt470bg".to_owned()),
                specified: true,
            },
            transfer: VideoColorTag {
                code: 5,
                name: Some("bt470bg".to_owned()),
                specified: true,
            },
            matrix: VideoColorTag {
                code: 5,
                name: Some("bt470bg".to_owned()),
                specified: true,
            },
        };
        assert_eq!(
            pal.exact_cicp_candidate_for_sampling(sampling),
            Some(ColorSpace::Rec601Pal),
            "the closed numeric table must use FFmpeg's canonical BT.470BG tag name"
        );

        let mut wrong_name = valid.clone();
        wrong_name.primaries.name = Some("bt2020".to_owned());
        assert!(!wrong_name.has_closed_cicp_representation());
        assert_eq!(wrong_name.exact_cicp_candidate_for_sampling(sampling), None);

        let mut wrong_specified = valid;
        wrong_specified.matrix.specified = false;
        assert!(!wrong_specified.has_closed_cicp_representation());
        assert_eq!(
            wrong_specified.exact_cicp_candidate_for_sampling(sampling),
            None
        );
    }

    #[test]
    fn serde_derived_cicp_evidence_cannot_disagree_with_raw_probe_facts() {
        let mut stream = video_stream(true, 10, false);
        let metadata = VideoColorMetadata {
            primaries: VideoColorTag {
                code: 1,
                name: Some("bt709".to_owned()),
                specified: true,
            },
            transfer: VideoColorTag {
                code: 1,
                name: Some("bt709".to_owned()),
                specified: true,
            },
            matrix: VideoColorTag {
                code: 1,
                name: Some("bt709".to_owned()),
                specified: true,
            },
        };
        stream.color_metadata = Some(metadata.clone());
        stream.color_interpretation = DetectedColorInterpretation {
            candidate_color_space: Some(ColorSpace::Rec709),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::CicpTags,
            evidence: vec![VideoColorInterpretationEvidence::ExactCicpTags {
                primaries: metadata.primaries.clone(),
                transfer: metadata.transfer.clone(),
                matrix: metadata.matrix.clone(),
                detected_color_space: ColorSpace::Rec709,
            }],
            warnings: Vec::new(),
            user_overridable: true,
        };
        assert_eq!(stream.executable_color_space(), Some(ColorSpace::Rec709));

        stream.color_interpretation.candidate_color_space = Some(ColorSpace::Rec2020);
        stream.color_interpretation.evidence =
            vec![VideoColorInterpretationEvidence::ExactCicpTags {
                primaries: VideoColorTag {
                    code: 9,
                    name: Some("bt2020".to_owned()),
                    specified: true,
                },
                transfer: metadata.transfer,
                matrix: metadata.matrix,
                detected_color_space: ColorSpace::Rec2020,
            }];
        let serialized = serde_json::to_vec(&stream).expect("serialize forged stream");
        let decoded: VideoStreamInfo =
            serde_json::from_slice(&serialized).expect("deserialize forged stream");
        assert_eq!(decoded.executable_color_space(), None);
    }

    #[test]
    fn declaration_resolution_rejects_same_scope_conflict_but_merges_duplicates() {
        let sony = declared_color_hint();
        let duplicate = sony.clone();
        assert_eq!(
            resolve_video_color_metadata_declarations(&[sony.clone(), duplicate]),
            VideoColorMetadataDeclarationResolution::Unique {
                scope: VideoColorMetadataHintScope::Stream,
                color_space: ColorSpace::SonySLog3SGamut3Cine,
            }
        );

        let apple = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "com.apple.proapps.cameraLog".to_owned(),
            value: "Apple Log".to_owned(),
            detected_color_space: ColorSpace::AppleLogBt2020,
            authority: VideoColorMetadataHintAuthority::SourceDeclaration(
                VideoColorMetadataDeclaration::AppleProAppsCameraLog,
            ),
        };
        assert_eq!(
            resolve_video_color_metadata_declarations(&[sony.clone(), apple]),
            VideoColorMetadataDeclarationResolution::Conflicting {
                scope: VideoColorMetadataHintScope::Stream,
            }
        );

        let mut container_apple = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Container,
            key: "com.apple.proapps.cameraLog".to_owned(),
            value: "Apple Log".to_owned(),
            detected_color_space: ColorSpace::AppleLogBt2020,
            authority: VideoColorMetadataHintAuthority::SourceDeclaration(
                VideoColorMetadataDeclaration::AppleProAppsCameraLog,
            ),
        };
        assert_eq!(
            resolve_video_color_metadata_declarations(&[sony, container_apple.clone()]),
            VideoColorMetadataDeclarationResolution::Unique {
                scope: VideoColorMetadataHintScope::Stream,
                color_space: ColorSpace::SonySLog3SGamut3Cine,
            }
        );
        container_apple.scope = VideoColorMetadataHintScope::FileName;
        assert!(!container_apple.is_executable_declaration());
    }

    #[test]
    fn captured_revision_binds_object_and_filesystem_change_generation() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("source.mov");
        fs::write(&path, b"AAAA").expect("write source");

        let first = MediaFileFingerprint::capture(&path);
        assert!(first.authorizes_reuse());
        assert!(first.object_identity.is_some());
        assert!(first.change_stamp.is_some());

        thread::sleep(Duration::from_millis(2));
        // Filesystem timestamp granularity varies by platform and mount
        // options; keep rewriting until the revision evidence rotates or the
        // deadline proves it cannot.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let second = loop {
            fs::write(&path, b"BBBB").expect("replace same-length source bytes");
            let candidate = MediaFileFingerprint::capture(&path);
            if candidate != first {
                break candidate;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "filesystem change stamp never rotated"
            );
            thread::sleep(Duration::from_millis(10));
        };

        assert!(second.authorizes_reuse());
        assert_eq!(first.len, second.len);
        assert_ne!(
            first, second,
            "same-length source mutation must rotate revision evidence"
        );
    }

    #[test]
    fn incomplete_metadata_only_revision_cannot_authorize_windows_reuse() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("source.mov");
        fs::write(&path, b"source").expect("write source");
        let metadata = fs::metadata(&path).expect("source metadata");
        let fingerprint = MediaFileFingerprint::from_metadata(&metadata);

        #[cfg(windows)]
        assert!(!fingerprint.authorizes_reuse());
        #[cfg(unix)]
        assert!(fingerprint.authorizes_reuse());
    }
}
