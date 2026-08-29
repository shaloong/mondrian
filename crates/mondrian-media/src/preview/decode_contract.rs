//! Exact physical source and geometry contracts for Preview decode.
//!
//! These values deliberately exclude Asset identity, authoring interpretation,
//! scheduling priority, generation, and presentation demand. They describe only
//! the physical file/stream revision, exact source-local target, media color
//! conversion input, and the geometry/payload contract that the media Adapter
//! is allowed to execute.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use mondrian_core::{
    CameraRawAdapter, CameraRawInterpretation, ColorSpace, Resolution, SourceSampleTarget,
    SourceSamplingBoundary, TimelineTime,
};
use serde::{Deserialize, Serialize};

use super::{MediaFileFingerprint, PreviewHardwareDecodeRequest, PreviewSourceColorContract};
use crate::info::{PixelFormat, VideoCodec, VideoCodecProfile, VideoStreamInfo};
use crate::proxy::{
    ProxyArtifactManifest, ProxyEncodingProfile, PROXY_MANIFEST_VERSION,
    PROXY_PRIMARY_VIDEO_STREAM_INDEX,
};

/// Versioned Camera RAW development identity shared by Preview and Export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CameraRawDecodeIntent {
    /// Source Adapter proven by the media probe.
    adapter: CameraRawAdapter,
    /// Persistent author controls.
    interpretation: CameraRawInterpretation,
    /// Exact algorithm revision included in every decode/cache identity.
    algorithm_version: u16,
}

impl CameraRawDecodeIntent {
    /// Current deterministic DNG development algorithm revision.
    pub const ALGORITHM_VERSION: u16 = 1;

    /// Build and validate an executable RAW development identity.
    pub fn new(
        adapter: CameraRawAdapter,
        interpretation: CameraRawInterpretation,
    ) -> Result<Self, PreviewDecodeContractError> {
        interpretation.validate().map_err(|error| {
            PreviewDecodeContractError::InvalidCameraRawInterpretation { reason: error.to_string() }
        })?;
        Ok(Self {
            adapter,
            interpretation,
            algorithm_version: Self::ALGORITHM_VERSION,
        })
    }

    /// Source Adapter proven by the media probe.
    pub const fn adapter(self) -> CameraRawAdapter {
        self.adapter
    }

    /// Persistent author controls carried by this exact execution identity.
    pub const fn interpretation(self) -> CameraRawInterpretation {
        self.interpretation
    }

    /// Exact development algorithm revision.
    pub const fn algorithm_version(self) -> u16 {
        self.algorithm_version
    }

    fn validate_current(self) -> Result<(), PreviewDecodeContractError> {
        self.interpretation.validate().map_err(|error| {
            PreviewDecodeContractError::InvalidCameraRawInterpretation { reason: error.to_string() }
        })?;
        if self.algorithm_version != Self::ALGORITHM_VERSION {
            return Err(
                PreviewDecodeContractError::UnsupportedCameraRawAlgorithmVersion {
                    algorithm_version: self.algorithm_version,
                },
            );
        }
        Ok(())
    }
}

/// Decoder-native surface family conservatively inferred from physical source evidence.
///
/// This is an admission hint, not proof that a decoder or renderer produced or
/// imported such a surface. Actual decoded frames retain their independent
/// [`crate::DecodedVideoSurfaceFormat`] evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewNativeSurfaceHint {
    /// An opaque 8-bit 4:2:0 source may decode to an NV12 hardware surface.
    Nv12,
    /// An opaque 10-bit 4:2:0 source may decode to a P010 hardware surface.
    P010,
    /// A 12-bit 4:2:0 source may decode to a P012-family hardware surface.
    Yuv420p12,
    /// A 16-bit 4:2:0 source may decode to a P016-family hardware surface.
    Yuv420p16,
    /// A 10-bit 4:2:2 source may decode to a planar or packed platform surface.
    Yuv422p10,
    /// A 12-bit 4:2:2 source may decode to a planar or packed platform surface.
    Yuv422p12,
    /// A 16-bit 4:2:2 source may decode to a platform-native surface.
    Yuv422p16,
    /// A 10-bit 4:4:4 source may decode to a planar or packed platform surface.
    Yuv444p10,
    /// A 12-bit 4:4:4 source may decode to a planar or packed platform surface.
    Yuv444p12,
    /// A 16-bit 4:4:4 source may decode to a platform-native surface.
    Yuv444p16,
}

/// Compact CPU YUV representation conservatively inferred from a probed source.
///
/// This is planning evidence only. The decoded frame remains authoritative and
/// may still fall back to another CPU representation when FFmpeg produces a
/// different surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewCompactCpuYuvHint {
    /// Planar little-endian 10-bit 4:2:2 retained as immutable Y, Cb, and Cr planes.
    Yuv422p10le,
}

impl PreviewCompactCpuYuvHint {
    /// Tightly packed retained bytes per luma pixel.
    pub const fn retained_bytes_per_pixel(self) -> usize {
        match self {
            Self::Yuv422p10le => 4,
        }
    }

    /// Conservative retained FFmpeg plane bytes for one materialized extent.
    ///
    /// Software decoders may align each plane row beyond the visible raster.
    /// Reserving 256-byte row alignment keeps admission valid across SIMD and
    /// codec-specific allocation policies while avoiding a full-frame copy
    /// merely to make the allocation tightly packed.
    pub const fn retained_bytes_for_extent(self, extent: Resolution) -> usize {
        match self {
            Self::Yuv422p10le => {
                let luma_row = align_up_saturating((extent.width as usize).saturating_mul(2), 256);
                let chroma_row =
                    align_up_saturating((extent.width.div_ceil(2) as usize).saturating_mul(2), 256);
                luma_row
                    .saturating_add(chroma_row.saturating_mul(2))
                    .saturating_mul(extent.height as usize)
            }
        }
    }
}

const fn align_up_saturating(value: usize, alignment: usize) -> usize {
    if alignment == 0 {
        return value;
    }
    let units = value.saturating_add(alignment.saturating_sub(1)) / alignment;
    units.saturating_mul(alignment)
}

/// Strength of physical Alpha evidence attached to one decode source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewDecodeAlphaPresence {
    /// Probe or generated-artifact evidence proves that the source is opaque.
    Opaque,
    /// Probe evidence proves that the source carries Alpha.
    Present,
    /// The caller has no complete sampling evidence; native output is forbidden.
    Unknown,
}

/// Exact selected absolute file path, revision, and physical video stream.
///
/// Construction requires a process-independent absolute path plus complete
/// filesystem object/change evidence. A path, length, or modification timestamp
/// alone can never become a reusable Preview decode identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewDecodeSource {
    path: PathBuf,
    fingerprint: MediaFileFingerprint,
    video_stream_index: u32,
    alpha_presence: PreviewDecodeAlphaPresence,
    native_surface_hint: Option<PreviewNativeSurfaceHint>,
    compact_cpu_yuv_hint: Option<PreviewCompactCpuYuvHint>,
    source_extent: Resolution,
}

impl PreviewDecodeSource {
    /// Capture and validate one original media source from an admitted video-stream probe.
    pub fn capture_probed_stream(
        path: impl Into<PathBuf>,
        stream: &VideoStreamInfo,
    ) -> Result<Self, PreviewDecodeContractError> {
        let path = path.into();
        let fingerprint = MediaFileFingerprint::capture(&path);
        Self::from_probed_stream(path, fingerprint, stream)
    }

    /// Build one original media source from an already captured exact revision and probe.
    pub fn from_probed_stream(
        path: impl Into<PathBuf>,
        fingerprint: MediaFileFingerprint,
        stream: &VideoStreamInfo,
    ) -> Result<Self, PreviewDecodeContractError> {
        if stream.width == 0 || stream.height == 0 {
            return Err(PreviewDecodeContractError::EmptySourceExtent {
                stream_index: stream.index,
                width: stream.width,
                height: stream.height,
            });
        }
        let sampling =
            stream
                .proven_sampling()
                .ok_or(PreviewDecodeContractError::UnprovenSourceSampling {
                    stream_index: stream.index,
                })?;
        let alpha_presence = if sampling.has_alpha {
            PreviewDecodeAlphaPresence::Present
        } else {
            PreviewDecodeAlphaPresence::Opaque
        };
        Self::new(
            path.into(),
            fingerprint,
            stream.index,
            alpha_presence,
            native_surface_hint_from_stream(stream, sampling.pixel_format),
            compact_cpu_yuv_hint_from_pixel_format(sampling.pixel_format),
            Resolution { width: stream.width, height: stream.height },
        )
    }

    /// Capture and validate one generated proxy artifact under its exact manifest contract.
    ///
    /// Mondrian proxy publication maps the first selected picture to output
    /// stream zero. Its native surface hint comes from the artifact encoding
    /// profile, never from the original source probe.
    pub fn capture_proxy_artifact(
        path: impl Into<PathBuf>,
        manifest: &ProxyArtifactManifest,
        source_extent: Resolution,
    ) -> Result<Self, PreviewDecodeContractError> {
        let path = path.into();
        let fingerprint = MediaFileFingerprint::capture(&path);
        Self::from_proxy_artifact(path, fingerprint, manifest, source_extent)
    }

    /// Build one generated proxy source from an already captured exact revision.
    pub fn from_proxy_artifact(
        path: impl Into<PathBuf>,
        fingerprint: MediaFileFingerprint,
        manifest: &ProxyArtifactManifest,
        source_extent: Resolution,
    ) -> Result<Self, PreviewDecodeContractError> {
        if manifest.version != PROXY_MANIFEST_VERSION {
            return Err(
                PreviewDecodeContractError::UnsupportedProxyManifestVersion {
                    actual: manifest.version,
                    expected: PROXY_MANIFEST_VERSION,
                },
            );
        }
        let native_surface_hint = match manifest.encoding {
            ProxyEncodingProfile::H264High8 => Some(PreviewNativeSurfaceHint::Nv12),
            ProxyEncodingProfile::H265Main10 => Some(PreviewNativeSurfaceHint::P010),
            ProxyEncodingProfile::DnxHrSq8 | ProxyEncodingProfile::DnxHrHqx10 => None,
        };
        Self::new(
            path.into(),
            fingerprint,
            PROXY_PRIMARY_VIDEO_STREAM_INDEX,
            PreviewDecodeAlphaPresence::Opaque,
            native_surface_hint,
            match manifest.encoding {
                ProxyEncodingProfile::DnxHrHqx10 => Some(PreviewCompactCpuYuvHint::Yuv422p10le),
                ProxyEncodingProfile::H264High8
                | ProxyEncodingProfile::H265Main10
                | ProxyEncodingProfile::DnxHrSq8 => None,
            },
            source_extent,
        )
    }

    /// Build an exact CPU-only source when no complete sampling evidence was frozen.
    ///
    /// This constructor is intended for callers such as immutable Export
    /// snapshots that already froze path/revision/stream but deliberately do
    /// not claim native-surface eligibility. `source_extent` is the frozen
    /// stream's own raster extent, never a consumer/output extent.
    pub fn from_frozen_cpu_stream(
        path: impl Into<PathBuf>,
        fingerprint: MediaFileFingerprint,
        video_stream_index: u32,
        source_extent: Resolution,
    ) -> Result<Self, PreviewDecodeContractError> {
        Self::new(
            path.into(),
            fingerprint,
            video_stream_index,
            PreviewDecodeAlphaPresence::Unknown,
            None,
            None,
            source_extent,
        )
    }

    fn new(
        path: PathBuf,
        fingerprint: MediaFileFingerprint,
        video_stream_index: u32,
        alpha_presence: PreviewDecodeAlphaPresence,
        native_surface_hint: Option<PreviewNativeSurfaceHint>,
        compact_cpu_yuv_hint: Option<PreviewCompactCpuYuvHint>,
        source_extent: Resolution,
    ) -> Result<Self, PreviewDecodeContractError> {
        if path.as_os_str().is_empty() {
            return Err(PreviewDecodeContractError::EmptySourcePath);
        }
        if !path.is_absolute() {
            return Err(PreviewDecodeContractError::RelativeSourcePath {
                path: path.display().to_string(),
            });
        }
        if !fingerprint.authorizes_reuse() {
            return Err(PreviewDecodeContractError::IncompleteSourceRevision {
                path: path.display().to_string(),
            });
        }
        validate_extent(source_extent)?;
        Ok(Self {
            path,
            fingerprint,
            video_stream_index,
            alpha_presence,
            native_surface_hint,
            compact_cpu_yuv_hint,
            source_extent,
        })
    }

    /// Selected physical file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Complete file revision that must still match at execution.
    pub const fn fingerprint(&self) -> MediaFileFingerprint {
        self.fingerprint
    }

    /// Absolute physical video stream selected from the container.
    pub const fn video_stream_index(&self) -> u32 {
        self.video_stream_index
    }

    /// Proven or explicitly unknown physical Alpha presence.
    pub const fn alpha_presence(&self) -> PreviewDecodeAlphaPresence {
        self.alpha_presence
    }

    /// Conservative native decoder surface family, when proven.
    pub const fn native_surface_hint(&self) -> Option<PreviewNativeSurfaceHint> {
        self.native_surface_hint
    }

    /// Compact CPU YUV layout expected from the selected physical stream.
    pub const fn compact_cpu_yuv_hint(&self) -> Option<PreviewCompactCpuYuvHint> {
        self.compact_cpu_yuv_hint
    }

    /// The source's own raster extent (never a consumer/output extent).
    pub const fn source_extent(&self) -> Resolution {
        self.source_extent
    }

    /// Whether this source is eligible to request an opaque native output.
    pub const fn permits_native_output(&self) -> bool {
        matches!(self.alpha_presence, PreviewDecodeAlphaPresence::Opaque)
            && self.native_surface_hint.is_some()
    }
}

/// Working representation quality requested by one Preview decode consumer.
///
/// This is a decode-policy input owned by the caller (Playback quality policy,
/// still-frame refinement, export). It selects the raster quality at which the
/// media's own representation is materialized; it is never a consumer/output
/// extent. `Full` keeps the source raster, while `Reduced { divisor }` bounds
/// the working raster to the source extent divided by `divisor` in both
/// dimensions (about 1/divisor^2 of the source pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PreviewRepresentationQuality {
    /// Media's own representation at its source raster extent.
    #[default]
    Full,
    /// Reduced working raster at a bounded divisor of the source extent.
    Reduced { divisor: NonZeroU32 },
}

/// Media representation identity for Preview decode, fully decoupled from
/// every consumer/output resolution.
///
/// The representation is a decode-policy choice owned by the media layer.
/// Preview/sequence/export output dimensions are never part of this identity:
/// an output-extent change is a composition/spatial-target change and must not
/// invalidate the decoded-frame cache. A runtime recovery policy may instead
/// request an explicit `Reduced` representation, which intentionally rotates
/// the cache identity. The compositor and spatial stages own scaling from the
/// selected representation to the composition target, with the same contract
/// on GPU and on CPU fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewDecodeRepresentation {
    /// Media's own representation at its source raster extent, materialized as
    /// CPU-addressable pixels. Decode never swscales to an output extent.
    NativeCpu,
    /// Media's source raster retained as compact CPU YUV planes for direct
    /// Renderer upload and GPU color materialization.
    ///
    /// This identity is admitted only from an exact probed layout and a
    /// GPU-capable downstream payload contract. A decoder format mismatch is
    /// an execution error; it cannot silently expand into an RGBA payload
    /// under the same cache and residency identity.
    CompactCpuYuv,
    /// Reduced-raster compact CPU YUV planes for realtime recovery.
    ///
    /// The divisor is physical cache/materialization identity, while the
    /// underlying compressed-stream decoder remains reusable across Full,
    /// Half, and Quarter representation changes.
    ReducedCompactCpuYuv { divisor: NonZeroU32 },
    /// Media's own representation as a decoder-native surface at source
    /// extent; the renderer materializes the composition target from it.
    NativeSurface,
    /// A generated proxy artifact representation at the artifact's own raster
    /// extent. This is a decode-policy seam (the artifact itself), not an
    /// output-resolution alias.
    Proxy(Resolution),
    /// Reduced-raster decode at a bounded divisor of the source extent.
    ///
    /// The representation keeps the media's own quality contract — never a
    /// consumer/output extent — but decodes a smaller working raster when the
    /// realtime pipeline cannot afford the source-resolution representation.
    /// Both raster identities can be resident at the same time: a cache may
    /// hold the same clip at `Full` and `Reduced { divisor: 2 }` without one
    /// invalidating the other, exactly as it holds `NativeCpu` and
    /// `NativeSurface` for the same source today.
    Reduced { divisor: NonZeroU32 },
}

impl PreviewDecodeRepresentation {
    /// Resolve the only valid representation for one payload requirement,
    /// hardware intent, and representation quality.
    ///
    /// No output extent participates: the representation is derived from the
    /// source, hardware admission, and the requested working quality alone.
    pub fn canonical(
        source: &PreviewDecodeSource,
        payload_requirement: PreviewDecodePayloadRequirement,
        hardware_request: PreviewHardwareDecodeRequest,
        representation_quality: PreviewRepresentationQuality,
        source_color: PreviewSourceColorContract,
    ) -> Result<Self, PreviewDecodeContractError> {
        let native_requested = matches!(
            hardware_request,
            PreviewHardwareDecodeRequest::PreferGpuResident
                | PreviewHardwareDecodeRequest::RequireGpuResident
        );
        if payload_requirement == PreviewDecodePayloadRequirement::NativeAllowed
            && native_requested
            && source.permits_native_output()
            && !source_color.is_data_texture()
        {
            return Ok(Self::NativeSurface);
        }
        if hardware_request == PreviewHardwareDecodeRequest::RequireGpuResident {
            return Err(
                PreviewDecodeContractError::RequiredNativeOutputUnavailable {
                    alpha_presence: source.alpha_presence,
                    native_surface_hint: source.native_surface_hint,
                    payload_requirement,
                },
            );
        }
        match representation_quality {
            PreviewRepresentationQuality::Full
                if payload_requirement == PreviewDecodePayloadRequirement::NativeAllowed
                    && source.compact_cpu_yuv_hint().is_some()
                    && !source_color.is_scene_linear()
                    && !source_color.is_data_texture() =>
            {
                Ok(Self::CompactCpuYuv)
            }
            PreviewRepresentationQuality::Full => Ok(Self::NativeCpu),
            PreviewRepresentationQuality::Reduced { divisor } if divisor.get() == 1 => {
                Err(PreviewDecodeContractError::IdentityReducedRepresentation)
            }
            PreviewRepresentationQuality::Reduced { divisor }
                if payload_requirement == PreviewDecodePayloadRequirement::NativeAllowed
                    && source.compact_cpu_yuv_hint().is_some()
                    && !source_color.is_scene_linear()
                    && !source_color.is_data_texture() =>
            {
                Ok(Self::ReducedCompactCpuYuv { divisor })
            }
            PreviewRepresentationQuality::Reduced { divisor } => Ok(Self::Reduced { divisor }),
        }
    }

    /// Resolve the representation for a generated proxy artifact at its own
    /// raster extent. The artifact itself is the identity; no output extent.
    pub fn for_proxy_artifact(extent: Resolution) -> Result<Self, PreviewDecodeContractError> {
        validate_extent(extent)?;
        Ok(Self::Proxy(extent))
    }

    /// Whether this representation may be carried as a decoder-native surface.
    pub const fn is_native_surface(self) -> bool {
        matches!(self, Self::NativeSurface)
    }

    /// Whether this representation is CPU-addressable.
    pub const fn is_cpu_addressable(self) -> bool {
        !self.is_native_surface()
    }

    /// Whether this representation preserves exact compact CPU YUV planes.
    pub const fn is_compact_cpu_yuv(self) -> bool {
        matches!(
            self,
            Self::CompactCpuYuv | Self::ReducedCompactCpuYuv { .. }
        )
    }

    /// The representation's own raster extent given a concrete source extent.
    ///
    /// Native representations keep the source extent; a reduced representation
    /// scales the source raster by its divisor; a proxy representation keeps
    /// its artifact extent. This is the decode output extent — never an
    /// output/composition extent.
    pub const fn extent_for_source(self, source: Resolution) -> Resolution {
        match self {
            Self::NativeCpu | Self::CompactCpuYuv | Self::NativeSurface => source,
            Self::Reduced { divisor } | Self::ReducedCompactCpuYuv { divisor } => {
                let divisor = divisor.get();
                Resolution {
                    width: source.width.div_ceil(divisor),
                    height: source.height.div_ceil(divisor),
                }
            }
            Self::Proxy(extent) => extent,
        }
    }

    /// Legacy FFmpeg maximum dimensions for the representation given a source
    /// extent. Decode caps at the representation extent, never the consumer's
    /// output extent.
    pub fn maximum_dimensions_for_source(self, source: Resolution) -> (Option<u32>, Option<u32>) {
        let extent = self.extent_for_source(source);
        (Some(extent.width), Some(extent.height))
    }

    /// The representation's materialization extent for a concrete source
    /// raster, aspect-preserved within the representation extent.
    pub fn materialization_extent_for_source(self, source: Resolution) -> Resolution {
        self.extent_for_source(source)
    }
}

/// Addressability required by the downstream Preview execution path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewDecodePayloadRequirement {
    /// The downstream processor must be able to address CPU pixels.
    CpuAddressable,
    /// A renderer-admitted native surface may be returned.
    NativeAllowed,
}

/// Exact cache/session-independent identity of one physical Preview decode.
///
/// The key contains no consumer/output extent: the representation is a
/// decode-policy identity, so an authored Viewer scale, Viewer size, or
/// sequence-output change never invalidates the decoded frame behind it.
/// Runtime recovery can still choose a different explicit representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewDecodeKey {
    source: PreviewDecodeSource,
    source_sample: SourceSampleTarget,
    representation: PreviewDecodeRepresentation,
    source_color: PreviewSourceColorContract,
    camera_raw: Option<CameraRawDecodeIntent>,
}

impl PreviewDecodeKey {
    /// Build a validated exact physical Preview decode key.
    pub fn new(
        source: PreviewDecodeSource,
        source_sample: SourceSampleTarget,
        representation: PreviewDecodeRepresentation,
        source_color: PreviewSourceColorContract,
    ) -> Result<Self, PreviewDecodeContractError> {
        if source_sample.time().is_negative() {
            return Err(PreviewDecodeContractError::NegativeSourceTime {
                source_time: source_sample.time(),
            });
        }
        if source_sample.time().is_zero()
            && source_sample.boundary() == SourceSamplingBoundary::StrictPredecessor
        {
            return Err(PreviewDecodeContractError::SourceSampleBeforeOrigin {
                source_time: source_sample.time(),
                boundary: source_sample.boundary(),
            });
        }
        validate_representation_for_source(representation, &source, source_color)?;
        Ok(Self {
            source,
            source_sample,
            representation,
            source_color,
            camera_raw: None,
        })
    }

    /// Build a validated exact decode key for a probe-admitted camera RAW source.
    pub fn new_camera_raw(
        source: PreviewDecodeSource,
        source_sample: SourceSampleTarget,
        representation: PreviewDecodeRepresentation,
        source_color: PreviewSourceColorContract,
        camera_raw: CameraRawDecodeIntent,
    ) -> Result<Self, PreviewDecodeContractError> {
        camera_raw.validate_current()?;
        if representation.is_native_surface() || representation.is_compact_cpu_yuv() {
            return Err(PreviewDecodeContractError::CameraRawRequiresCpuFloat);
        }
        if source_color.color_space() != Some(ColorSpace::LinearRec709) {
            return Err(PreviewDecodeContractError::CameraRawRequiresLinearRec709 {
                actual: source_color.color_space(),
            });
        }
        let mut key = Self::new(source, source_sample, representation, source_color)?;
        key.camera_raw = Some(camera_raw);
        Ok(key)
    }

    /// Selected physical file and stream revision.
    pub const fn source(&self) -> &PreviewDecodeSource {
        &self.source
    }

    /// Exact media-source-local target.
    pub const fn source_sample(&self) -> SourceSampleTarget {
        self.source_sample
    }

    /// Decode-policy representation identity (no output extent).
    pub const fn representation(&self) -> PreviewDecodeRepresentation {
        self.representation
    }

    /// App-resolved source color/range facts used by media conversion.
    pub const fn source_color(&self) -> PreviewSourceColorContract {
        self.source_color
    }

    /// Camera RAW development identity, when the probe admitted one.
    pub const fn camera_raw(&self) -> Option<CameraRawDecodeIntent> {
        self.camera_raw
    }
}

fn validate_representation_for_source(
    representation: PreviewDecodeRepresentation,
    source: &PreviewDecodeSource,
    source_color: PreviewSourceColorContract,
) -> Result<(), PreviewDecodeContractError> {
    match representation {
        PreviewDecodeRepresentation::NativeSurface => {
            if source_color.is_data_texture() {
                Err(PreviewDecodeContractError::DataTextureRequiresCpuRgb)
            } else if source.permits_native_output() {
                Ok(())
            } else {
                Err(PreviewDecodeContractError::NativeSourceUnavailable {
                    alpha_presence: source.alpha_presence,
                    native_surface_hint: source.native_surface_hint,
                })
            }
        }
        PreviewDecodeRepresentation::Reduced { divisor }
        | PreviewDecodeRepresentation::ReducedCompactCpuYuv { divisor }
            if divisor.get() == 1 =>
        {
            Err(PreviewDecodeContractError::IdentityReducedRepresentation)
        }
        PreviewDecodeRepresentation::CompactCpuYuv
        | PreviewDecodeRepresentation::ReducedCompactCpuYuv { .. } => {
            if source_color.is_data_texture() {
                Err(PreviewDecodeContractError::DataTextureRequiresCpuRgb)
            } else if source.compact_cpu_yuv_hint().is_some() && !source_color.is_scene_linear() {
                Ok(())
            } else {
                Err(PreviewDecodeContractError::CompactCpuYuvUnavailable {
                    compact_hint: source.compact_cpu_yuv_hint(),
                    source_color_space: source_color.color_space(),
                })
            }
        }
        PreviewDecodeRepresentation::Proxy(extent) => {
            if source_color.is_data_texture() {
                Err(PreviewDecodeContractError::DataTextureRequiresCpuRgb)
            } else {
                validate_extent(extent)
            }
        }
        PreviewDecodeRepresentation::NativeCpu | PreviewDecodeRepresentation::Reduced { .. } => {
            Ok(())
        }
    }
}

/// Invalid or incomplete physical Preview decode contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreviewDecodeContractError {
    /// RAW author controls failed their closed bounds.
    #[error("invalid camera RAW interpretation: {reason}")]
    InvalidCameraRawInterpretation {
        /// Stable validation reason.
        reason: String,
    },
    /// Serialized or external state selected a development revision this build cannot execute.
    #[error("unsupported camera RAW algorithm version {algorithm_version}")]
    UnsupportedCameraRawAlgorithmVersion {
        /// Unrecognized development revision.
        algorithm_version: u16,
    },
    /// Camera RAW output is always CPU-addressable scene-linear float in this slice.
    #[error("camera RAW Preview decode requires CPU float output")]
    CameraRawRequiresCpuFloat,
    /// Camera RAW Adapter output has one explicit source identity.
    #[error("camera RAW Preview decode requires LinearRec709 source identity, got {actual:?}")]
    CameraRawRequiresLinearRec709 {
        /// Contradictory input identity.
        actual: Option<ColorSpace>,
    },
    /// Empty paths cannot identify a physical source.
    #[error("Preview decode source path is empty")]
    EmptySourcePath,
    /// A reusable physical identity cannot depend on the process working directory.
    #[error("Preview decode source path is not absolute: {path}")]
    RelativeSourcePath {
        /// Relative path rejected by the contract.
        path: String,
    },
    /// The filesystem did not provide complete object/change evidence.
    #[error("Preview decode source revision is incomplete for {path}")]
    IncompleteSourceRevision {
        /// Source path whose revision was incomplete.
        path: String,
    },
    /// The probed stream had no non-empty physical raster.
    #[error("Preview video stream {stream_index} has an empty source extent {width}x{height}")]
    EmptySourceExtent {
        /// Absolute physical stream index.
        stream_index: u32,
        /// Invalid width.
        width: u32,
        /// Invalid height.
        height: u32,
    },
    /// Sampling fallbacks cannot authorize Alpha or native-surface behavior.
    #[error("Preview video stream {stream_index} has no proven sampling contract")]
    UnprovenSourceSampling {
        /// Absolute physical stream index.
        stream_index: u32,
    },
    /// Only the current generated-proxy manifest contract is executable.
    #[error("Preview proxy manifest version {actual} is unsupported; expected {expected}")]
    UnsupportedProxyManifestVersion {
        /// Observed manifest version.
        actual: u16,
        /// Current executable manifest version.
        expected: u16,
    },
    /// CPU geometry must be non-empty.
    #[error("Preview decode geometry is empty: {width}x{height}")]
    EmptyDecodeExtent {
        /// Invalid width.
        width: u32,
        /// Invalid height.
        height: u32,
    },
    /// Divisor one duplicates the canonical full CPU representation.
    #[error("Preview reduced representation divisor must be greater than one")]
    IdentityReducedRepresentation,
    /// Native geometry requires an opaque source with a supported surface hint.
    #[error(
        "source-native Preview geometry is unavailable for alpha={alpha_presence:?}, surface={native_surface_hint:?}"
    )]
    NativeSourceUnavailable {
        /// Physical Alpha evidence.
        alpha_presence: PreviewDecodeAlphaPresence,
        /// Physical native-surface hint.
        native_surface_hint: Option<PreviewNativeSurfaceHint>,
    },
    /// Compact CPU YUV requires an exact physical layout and nonlinear source encoding.
    #[error(
        "compact CPU YUV Preview output is unavailable for hint={compact_hint:?}, color={source_color_space:?}"
    )]
    CompactCpuYuvUnavailable {
        /// Exact physical compact-layout evidence, when present.
        compact_hint: Option<PreviewCompactCpuYuvHint>,
        /// Resolved source color identity.
        source_color_space: Option<mondrian_core::ColorSpace>,
    },
    /// Data textures require CPU-addressable RGB samples so no YCbCr or native
    /// color conversion can silently alter their numeric channels.
    #[error("data-texture Preview decode requires CPU-addressable RGB output")]
    DataTextureRequiresCpuRgb,
    /// A required native request cannot be reconciled with source/output facts.
    #[error(
        "required native Preview output is unavailable for requirement={payload_requirement:?}, alpha={alpha_presence:?}, surface={native_surface_hint:?}"
    )]
    RequiredNativeOutputUnavailable {
        /// Physical Alpha evidence.
        alpha_presence: PreviewDecodeAlphaPresence,
        /// Physical native-surface hint.
        native_surface_hint: Option<PreviewNativeSurfaceHint>,
        /// Downstream payload requirement.
        payload_requirement: PreviewDecodePayloadRequirement,
    },
    /// Physical decode targets are nonnegative.
    #[error("Preview source target is negative: {source_time}")]
    NegativeSourceTime {
        /// Invalid source-local target.
        source_time: TimelineTime,
    },
    /// A strict-predecessor request at source origin has no physical sample.
    #[error("Preview source sample precedes origin: time={source_time}, boundary={boundary:?}")]
    SourceSampleBeforeOrigin {
        /// Exact source-local boundary.
        source_time: TimelineTime,
        /// Sampling boundary that selected the missing predecessor.
        boundary: SourceSamplingBoundary,
    },
}

fn validate_extent(extent: Resolution) -> Result<(), PreviewDecodeContractError> {
    if extent.width == 0 || extent.height == 0 {
        return Err(PreviewDecodeContractError::EmptyDecodeExtent {
            width: extent.width,
            height: extent.height,
        });
    }
    Ok(())
}

const fn native_surface_hint_from_pixel_format(
    pixel_format: PixelFormat,
) -> Option<PreviewNativeSurfaceHint> {
    match pixel_format {
        PixelFormat::Yuv420p | PixelFormat::Nv12 => Some(PreviewNativeSurfaceHint::Nv12),
        PixelFormat::Yuv420p10le | PixelFormat::P010 => Some(PreviewNativeSurfaceHint::P010),
        PixelFormat::Yuv420p12le | PixelFormat::P012 => Some(PreviewNativeSurfaceHint::Yuv420p12),
        PixelFormat::Yuv420p16le | PixelFormat::P016 => Some(PreviewNativeSurfaceHint::Yuv420p16),
        PixelFormat::Yuv422p10le => Some(PreviewNativeSurfaceHint::Yuv422p10),
        PixelFormat::Yuv422p12le => Some(PreviewNativeSurfaceHint::Yuv422p12),
        PixelFormat::Yuv422p16le => Some(PreviewNativeSurfaceHint::Yuv422p16),
        PixelFormat::Yuv444p10le => Some(PreviewNativeSurfaceHint::Yuv444p10),
        PixelFormat::Yuv444p12le => Some(PreviewNativeSurfaceHint::Yuv444p12),
        PixelFormat::Yuv444p16le => Some(PreviewNativeSurfaceHint::Yuv444p16),
        PixelFormat::Yuv422p
        | PixelFormat::Yuv444p
        | PixelFormat::Gbrp10le
        | PixelFormat::Gbrp12le
        | PixelFormat::Gbrp16le
        | PixelFormat::Gbrap10le
        | PixelFormat::Gbrap12le
        | PixelFormat::Gbrap16le
        | PixelFormat::Rgb24
        | PixelFormat::Rgba
        | PixelFormat::Rgba64le
        | PixelFormat::BayerRggb8
        | PixelFormat::BayerBggr8
        | PixelFormat::BayerGbrg8
        | PixelFormat::BayerGrbg8
        | PixelFormat::BayerRggb16le
        | PixelFormat::BayerBggr16le
        | PixelFormat::BayerGbrg16le
        | PixelFormat::BayerGrbg16le => None,
    }
}

const fn compact_cpu_yuv_hint_from_pixel_format(
    pixel_format: PixelFormat,
) -> Option<PreviewCompactCpuYuvHint> {
    match pixel_format {
        PixelFormat::Yuv422p10le => Some(PreviewCompactCpuYuvHint::Yuv422p10le),
        _ => None,
    }
}

fn native_surface_hint_from_stream(
    stream: &VideoStreamInfo,
    pixel_format: PixelFormat,
) -> Option<PreviewNativeSurfaceHint> {
    let surface = native_surface_hint_from_pixel_format(pixel_format)?;
    match surface {
        PreviewNativeSurfaceHint::Nv12 => match &stream.codec {
            VideoCodec::H264 => (!matches!(
                stream.codec_profile,
                VideoCodecProfile::H264High10
                    | VideoCodecProfile::H264High10Intra
                    | VideoCodecProfile::H264High422
                    | VideoCodecProfile::H264High422Intra
                    | VideoCodecProfile::H264High444
                    | VideoCodecProfile::H264High444Predictive
                    | VideoCodecProfile::H264High444Intra
                    | VideoCodecProfile::H264Cavlc444
            ))
            .then_some(surface),
            VideoCodec::H265 | VideoCodec::Av1 | VideoCodec::Vp9 => Some(surface),
            _ => None,
        },
        PreviewNativeSurfaceHint::P010 => match &stream.codec {
            VideoCodec::H265
                if matches!(
                    stream.codec_profile,
                    VideoCodecProfile::HevcMain10
                        | VideoCodecProfile::HevcRangeExtensions
                        | VideoCodecProfile::Unknown
                ) =>
            {
                Some(surface)
            }
            VideoCodec::Av1 | VideoCodec::Vp9 => Some(surface),
            _ => None,
        },
        PreviewNativeSurfaceHint::Yuv420p12
        | PreviewNativeSurfaceHint::Yuv420p16
        | PreviewNativeSurfaceHint::Yuv422p10
        | PreviewNativeSurfaceHint::Yuv422p12
        | PreviewNativeSurfaceHint::Yuv422p16
        | PreviewNativeSurfaceHint::Yuv444p10
        | PreviewNativeSurfaceHint::Yuv444p12
        | PreviewNativeSurfaceHint::Yuv444p16 => match &stream.codec {
            VideoCodec::H265
                if matches!(
                    stream.codec_profile,
                    VideoCodecProfile::HevcRangeExtensions | VideoCodecProfile::Unknown
                ) =>
            {
                Some(surface)
            }
            VideoCodec::Av1 | VideoCodec::Vp9 => Some(surface),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use mondrian_core::{
        ColorSpace, DetectedColorInterpretation, MediaFileChangeStamp, MediaFileObjectIdentity,
        Rational, VideoCodec, VideoCodecProfile,
    };

    use super::*;
    use crate::proxy::{
        ProxyArtifactSettings, ProxyColorContract, ProxyResolution, ProxySourceFingerprint,
    };
    use crate::DecodedVideoRange;

    fn exact_fingerprint(seed: u64) -> MediaFileFingerprint {
        MediaFileFingerprint {
            len: Some(seed.saturating_add(1)),
            modified_secs: Some(seed.saturating_add(2)),
            modified_nanos: Some((seed % u64::from(u32::MAX)) as u32),
            object_identity: Some(MediaFileObjectIdentity::Unix {
                device: seed.saturating_add(3),
                inode: seed.saturating_add(4),
            }),
            change_stamp: Some(MediaFileChangeStamp::Unix {
                seconds: seed as i64,
                nanoseconds: seed.saturating_add(5) as i64,
            }),
        }
    }

    fn absolute_test_path(label: impl AsRef<std::path::Path>) -> PathBuf {
        std::env::temp_dir().join("mondrian-preview-decode-contract").join(label)
    }

    fn video_stream(
        index: u32,
        pixel_format: PixelFormat,
        pixel_format_proven: bool,
    ) -> VideoStreamInfo {
        VideoStreamInfo {
            index,
            codec: VideoCodec::H265,
            duration: Some(Duration::from_secs(1)),
            codec_profile: VideoCodecProfile::HevcMain10,
            width: 3840,
            height: 2160,
            picture: mondrian_core::PictureStreamMetadata::default(),
            frame_rate: Rational::new(25, 1),
            frame_rate_proven: true,
            pixel_format,
            pixel_format_proven,
            color_range: DecodedVideoRange::Limited,
            color_interpretation: DetectedColorInterpretation::decoder_unavailable(),
            color_metadata: None,
            color_metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
            camera_raw: None,
            bit_depth: pixel_format.bit_depth(),
            has_alpha: pixel_format.has_alpha(),
            avg_bitrate: 1,
            total_frames: Some(25),
        }
    }

    fn proxy_manifest(encoding: ProxyEncodingProfile) -> ProxyArtifactManifest {
        ProxyArtifactManifest {
            version: PROXY_MANIFEST_VERSION,
            source: ProxySourceFingerprint {
                len: Some(1),
                modified_secs: Some(2),
                modified_nanos: Some(3),
                object_identity: Some(MediaFileObjectIdentity::Unix { device: 4, inode: 5 }),
                change_stamp: Some(MediaFileChangeStamp::Unix { seconds: 6, nanoseconds: 7 }),
            },
            color: ProxyColorContract::try_new(ColorSpace::Rec709, 8, DecodedVideoRange::Limited)
                .expect("valid proxy color"),
            settings: ProxyArtifactSettings { resolution: ProxyResolution::P720, crf: 20 },
            encoding,
        }
    }

    fn source_color() -> PreviewSourceColorContract {
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited)
    }

    #[test]
    fn probed_source_preserves_absolute_stream_and_sampling_hint() {
        let stream = video_stream(7, PixelFormat::P010, true);
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(10),
            &stream,
        )
        .expect("valid probed source");

        assert_eq!(source.video_stream_index(), 7);
        assert_eq!(
            source.native_surface_hint(),
            Some(PreviewNativeSurfaceHint::P010)
        );
        assert_eq!(source.alpha_presence(), PreviewDecodeAlphaPresence::Opaque);
    }

    #[test]
    fn native_surface_hint_rejects_codec_profile_false_positives() {
        let mut h264_high10 = video_stream(3, PixelFormat::Yuv420p10le, true);
        h264_high10.codec = VideoCodec::H264;
        h264_high10.codec_profile = VideoCodecProfile::H264High10;
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/h264-high10.mp4"),
            exact_fingerprint(14),
            &h264_high10,
        )
        .expect("H.264 High10 remains a valid CPU-decodable source");
        assert_eq!(source.native_surface_hint(), None);

        let mut hevc_main10 = video_stream(4, PixelFormat::Yuv420p10le, true);
        hevc_main10.codec = VideoCodec::H265;
        hevc_main10.codec_profile = VideoCodecProfile::HevcMain10;
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/hevc-main10.mp4"),
            exact_fingerprint(15),
            &hevc_main10,
        )
        .expect("HEVC Main10 remains eligible for P010 admission");
        assert_eq!(
            source.native_surface_hint(),
            Some(PreviewNativeSurfaceHint::P010)
        );

        let mut prores = video_stream(5, PixelFormat::Yuv420p, true);
        prores.codec = VideoCodec::ProRes(mondrian_core::ProResVariant::Proxy);
        prores.codec_profile = VideoCodecProfile::Unknown;
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/prores-proxy.mov"),
            exact_fingerprint(16),
            &prores,
        )
        .expect("ProRes remains a valid CPU-decodable source");
        assert_eq!(source.native_surface_hint(), None);
    }

    #[test]
    fn compact_cpu_yuv_hint_requires_exact_probed_layout() {
        let mut sony_422 = video_stream(8, PixelFormat::Yuv422p10le, true);
        sony_422.codec = VideoCodec::H264;
        sony_422.codec_profile = VideoCodecProfile::H264High422;
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/h264-high422.mp4"),
            exact_fingerprint(17),
            &sony_422,
        )
        .expect("H.264 High 4:2:2 source");
        assert_eq!(source.native_surface_hint(), None);
        assert_eq!(
            source.compact_cpu_yuv_hint(),
            Some(PreviewCompactCpuYuvHint::Yuv422p10le)
        );
        assert_eq!(
            source
                .compact_cpu_yuv_hint()
                .map(PreviewCompactCpuYuvHint::retained_bytes_per_pixel),
            Some(4)
        );
        assert_eq!(
            PreviewCompactCpuYuvHint::Yuv422p10le
                .retained_bytes_for_extent(Resolution { width: 3840, height: 2160 }),
            3840usize * 2160usize * 4
        );
        assert_eq!(
            PreviewCompactCpuYuvHint::Yuv422p10le
                .retained_bytes_for_extent(Resolution { width: 1921, height: 1080 }),
            8192usize * 1080usize
        );
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &source,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferHardwareDecode,
                PreviewRepresentationQuality::Full,
                source_color(),
            )
            .expect("GPU-capable Preview may retain compact software-decoded YUV"),
            PreviewDecodeRepresentation::CompactCpuYuv
        );
        let half_divisor = NonZeroU32::new(2).expect("divisor");
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &source,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferHardwareDecode,
                PreviewRepresentationQuality::Reduced { divisor: half_divisor },
                source_color(),
            )
            .expect("adaptive Preview must retain the compact YUV contract"),
            PreviewDecodeRepresentation::ReducedCompactCpuYuv { divisor: half_divisor }
        );
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &source,
                PreviewDecodePayloadRequirement::CpuAddressable,
                PreviewHardwareDecodeRequest::PreferHardwareDecode,
                PreviewRepresentationQuality::Full,
                source_color(),
            )
            .expect("CPU consumers require RGB-addressable pixels"),
            PreviewDecodeRepresentation::NativeCpu
        );
        let linear_source_color = PreviewSourceColorContract::automatic(
            ColorSpace::LinearRec709,
            DecodedVideoRange::Limited,
        );
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &source,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferHardwareDecode,
                PreviewRepresentationQuality::Full,
                linear_source_color,
            )
            .expect("scene-linear sources keep their float CPU representation"),
            PreviewDecodeRepresentation::NativeCpu
        );

        let yuv444 = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/yuv444p10.mov"),
            exact_fingerprint(18),
            &video_stream(9, PixelFormat::Yuv444p10le, true),
        )
        .expect("10-bit 4:4:4 source");
        assert_eq!(yuv444.compact_cpu_yuv_hint(), None);
    }

    #[test]
    fn unproven_sampling_incomplete_revision_and_relative_path_are_rejected() {
        let unproven = video_stream(2, PixelFormat::P010, false);
        assert!(matches!(
            PreviewDecodeSource::from_probed_stream(
                absolute_test_path("media/source.mov"),
                exact_fingerprint(11),
                &unproven,
            ),
            Err(PreviewDecodeContractError::UnprovenSourceSampling { stream_index: 2 })
        ));

        assert!(matches!(
            PreviewDecodeSource::from_frozen_cpu_stream(
                absolute_test_path("media/source.mov"),
                MediaFileFingerprint::default(),
                2,
                Resolution { width: 3840, height: 2160 },
            ),
            Err(PreviewDecodeContractError::IncompleteSourceRevision { .. })
        ));

        assert!(matches!(
            PreviewDecodeSource::from_frozen_cpu_stream(
                "relative/source.mov",
                exact_fingerprint(12),
                2,
                Resolution { width: 3840, height: 2160 },
            ),
            Err(PreviewDecodeContractError::RelativeSourcePath { .. })
        ));
    }

    #[test]
    fn proxy_uses_output_stream_zero_and_profile_surface_instead_of_source_facts() {
        let original = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(12),
            &video_stream(7, PixelFormat::P010, true),
        )
        .expect("valid original source");
        let proxy = PreviewDecodeSource::from_proxy_artifact(
            absolute_test_path("cache/proxy.mp4"),
            exact_fingerprint(13),
            &proxy_manifest(ProxyEncodingProfile::H264High8),
            Resolution { width: 1280, height: 720 },
        )
        .expect("valid proxy source");

        assert_eq!(original.video_stream_index(), 7);
        assert_eq!(
            original.native_surface_hint(),
            Some(PreviewNativeSurfaceHint::P010)
        );
        assert_eq!(proxy.video_stream_index(), 0);
        assert_eq!(
            proxy.native_surface_hint(),
            Some(PreviewNativeSurfaceHint::Nv12)
        );
    }

    #[test]
    fn every_proxy_profile_has_one_explicit_native_surface_projection() {
        let cases = [
            (
                ProxyEncodingProfile::H264High8,
                Some(PreviewNativeSurfaceHint::Nv12),
            ),
            (
                ProxyEncodingProfile::H265Main10,
                Some(PreviewNativeSurfaceHint::P010),
            ),
            (ProxyEncodingProfile::DnxHrSq8, None),
            (ProxyEncodingProfile::DnxHrHqx10, None),
        ];
        for (index, (encoding, expected)) in cases.into_iter().enumerate() {
            let source = PreviewDecodeSource::from_proxy_artifact(
                absolute_test_path(format!("cache/proxy-{index}.mov")),
                exact_fingerprint(index as u64 + 20),
                &proxy_manifest(encoding),
                Resolution { width: 1280, height: 720 },
            )
            .expect("valid proxy source");
            assert_eq!(source.video_stream_index(), 0);
            assert_eq!(source.native_surface_hint(), expected);
            assert_eq!(source.alpha_presence(), PreviewDecodeAlphaPresence::Opaque);
        }
    }

    #[test]
    fn canonical_representation_separates_cpu_and_native_contracts() {
        let native_source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(30),
            &video_stream(0, PixelFormat::P010, true),
        )
        .expect("valid native source");

        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &native_source,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferGpuResident,
                PreviewRepresentationQuality::Full,
                source_color(),
            )
            .expect("native representation"),
            PreviewDecodeRepresentation::NativeSurface
        );
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &native_source,
                PreviewDecodePayloadRequirement::CpuAddressable,
                PreviewHardwareDecodeRequest::PreferGpuResident,
                PreviewRepresentationQuality::Full,
                source_color(),
            )
            .expect("CPU representation"),
            PreviewDecodeRepresentation::NativeCpu
        );

        let alpha_source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/alpha.mov"),
            exact_fingerprint(31),
            &video_stream(0, PixelFormat::Rgba, true),
        )
        .expect("valid alpha source");
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &alpha_source,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferGpuResident,
                PreviewRepresentationQuality::Full,
                source_color(),
            )
            .expect("preferred native may safely downgrade"),
            PreviewDecodeRepresentation::NativeCpu
        );
        assert!(matches!(
            PreviewDecodeRepresentation::canonical(
                &alpha_source,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::RequireGpuResident,
                PreviewRepresentationQuality::Full,
                source_color(),
            ),
            Err(PreviewDecodeContractError::RequiredNativeOutputUnavailable { .. })
        ));
    }

    #[test]
    fn representation_extents_come_from_source_not_output() {
        let source_extent = Resolution { width: 3840, height: 2160 };
        let native = PreviewDecodeRepresentation::NativeCpu;
        assert_eq!(
            native.maximum_dimensions_for_source(source_extent),
            (Some(3840), Some(2160))
        );
        let proxy = PreviewDecodeRepresentation::Proxy(Resolution { width: 1280, height: 720 });
        assert_eq!(
            proxy.extent_for_source(source_extent),
            Resolution { width: 1280, height: 720 }
        );
        let half =
            PreviewDecodeRepresentation::Reduced { divisor: NonZeroU32::new(2).expect("divisor") };
        assert_eq!(
            half.extent_for_source(source_extent),
            Resolution { width: 1920, height: 1080 }
        );
        assert_eq!(
            half.maximum_dimensions_for_source(source_extent),
            (Some(1920), Some(1080))
        );
        let quarter =
            PreviewDecodeRepresentation::Reduced { divisor: NonZeroU32::new(4).expect("divisor") };
        assert_eq!(
            quarter.materialization_extent_for_source(source_extent),
            Resolution { width: 960, height: 540 }
        );
        assert!(
            half.is_cpu_addressable(),
            "reduced CPU representations remain CPU-addressable"
        );
        assert!(!half.is_native_surface());
        let compact_half = PreviewDecodeRepresentation::ReducedCompactCpuYuv {
            divisor: NonZeroU32::new(2).expect("divisor"),
        };
        assert_eq!(
            compact_half.extent_for_source(source_extent),
            half.extent_for_source(source_extent)
        );
        assert!(compact_half.is_compact_cpu_yuv());
    }

    #[test]
    fn reduced_quality_resolves_to_a_reduced_cpu_representation() {
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/reduced.mov"),
            exact_fingerprint(33),
            &video_stream(0, PixelFormat::Yuv420p, true),
        )
        .expect("valid source");
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &source,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::Auto,
                PreviewRepresentationQuality::Reduced {
                    divisor: NonZeroU32::new(2).expect("divisor"),
                },
                source_color(),
            )
            .expect("reduced CPU representation"),
            PreviewDecodeRepresentation::Reduced { divisor: NonZeroU32::new(2).expect("divisor") }
        );
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &source,
                PreviewDecodePayloadRequirement::CpuAddressable,
                PreviewHardwareDecodeRequest::Auto,
                PreviewRepresentationQuality::Reduced { divisor: NonZeroU32::MIN },
                source_color(),
            ),
            Err(PreviewDecodeContractError::IdentityReducedRepresentation)
        );
        // A native-capable source still prefers its decoder-native surface;
        // the reduced raster is a CPU quality fallback, not a native contract.
        let native_source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/native.mov"),
            exact_fingerprint(34),
            &video_stream(0, PixelFormat::Nv12, true),
        )
        .expect("valid native source");
        assert_eq!(
            PreviewDecodeRepresentation::canonical(
                &native_source,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferGpuResident,
                PreviewRepresentationQuality::Reduced {
                    divisor: NonZeroU32::new(4).expect("divisor"),
                },
                source_color(),
            )
            .expect("native surface preference wins over reduced quality"),
            PreviewDecodeRepresentation::NativeSurface
        );
    }

    #[test]
    fn full_and_reduced_representations_are_distinct_cache_identities() {
        let source_extent = Resolution { width: 3840, height: 2160 };
        let full = PreviewDecodeRepresentation::NativeCpu;
        let half =
            PreviewDecodeRepresentation::Reduced { divisor: NonZeroU32::new(2).expect("divisor") };
        let quarter =
            PreviewDecodeRepresentation::Reduced { divisor: NonZeroU32::new(4).expect("divisor") };
        assert_ne!(full, half);
        assert_ne!(half, quarter);
        assert_ne!(full, quarter);
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/identity.mov"),
            exact_fingerprint(35),
            &video_stream(0, PixelFormat::Yuv420p, true),
        )
        .expect("valid source");
        let color =
            PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited);
        let full_key = PreviewDecodeKey::new(
            source.clone(),
            SourceSampleTarget::covering(TimelineTime::new(0, 1).expect("origin")),
            full,
            color,
        )
        .expect("full key");
        let half_key = PreviewDecodeKey::new(
            source.clone(),
            SourceSampleTarget::covering(TimelineTime::new(0, 1).expect("origin")),
            half,
            color,
        )
        .expect("half key");
        assert_ne!(
            full_key, half_key,
            "Full and Reduced are distinct decode-policy identities and must cache independently"
        );
        assert_eq!(
            full_key.representation().extent_for_source(source_extent).width,
            3840
        );
        assert_eq!(
            half_key.representation().extent_for_source(source_extent).width,
            1920
        );
    }

    #[test]
    fn decode_key_rejects_duplicate_or_empty_representation_identities() {
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/invalid-representation.mov"),
            exact_fingerprint(36),
            &video_stream(0, PixelFormat::Yuv420p, true),
        )
        .expect("valid source");
        let sample = SourceSampleTarget::covering(TimelineTime::ZERO);
        let color =
            PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited);

        assert_eq!(
            PreviewDecodeKey::new(
                source.clone(),
                sample,
                PreviewDecodeRepresentation::Reduced { divisor: NonZeroU32::MIN },
                color,
            ),
            Err(PreviewDecodeContractError::IdentityReducedRepresentation)
        );
        assert_eq!(
            PreviewDecodeKey::new(
                source,
                sample,
                PreviewDecodeRepresentation::Proxy(Resolution { width: 0, height: 720 }),
                color,
            ),
            Err(PreviewDecodeContractError::EmptyDecodeExtent { width: 0, height: 720 })
        );
    }

    #[test]
    fn key_rejects_negative_time_and_legacy_request_projection_is_exact() {
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(40),
            &video_stream(5, PixelFormat::Yuv420p, true),
        )
        .expect("valid source");
        let representation = PreviewDecodeRepresentation::NativeCpu;
        assert!(matches!(
            PreviewDecodeKey::new(
                source.clone(),
                SourceSampleTarget::covering(
                    TimelineTime::new(-1, 1).expect("valid negative rational"),
                ),
                representation,
                source_color(),
            ),
            Err(PreviewDecodeContractError::NegativeSourceTime { .. })
        ));

        let key = PreviewDecodeKey::new(
            source,
            SourceSampleTarget::covering(TimelineTime::new(1, 2).expect("valid source time")),
            representation,
            source_color(),
        )
        .expect("valid decode key");
        let request = super::super::PreviewDecodeRequest::from_key(
            &key,
            super::super::PreviewDecodeAccessMode::PlaybackCursor,
        );

        assert_eq!(request.path, key.source().path());
        assert_eq!(
            request.video_stream_index,
            Some(key.source().video_stream_index())
        );
        assert_eq!(request.fingerprint, Some(key.source().fingerprint()));
        assert_eq!(request.source_sample, key.source_sample());
        assert_eq!(request.max_width, Some(3840));
        assert_eq!(request.max_height, Some(2160));
        assert_eq!(request.source_color, key.source_color());
    }

    #[test]
    fn key_identity_distinguishes_covering_and_strict_predecessor_targets() {
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(41),
            &video_stream(5, PixelFormat::Yuv420p, true),
        )
        .expect("valid source");
        let representation = PreviewDecodeRepresentation::NativeCpu;
        let source_time = TimelineTime::new(1, 1).expect("valid source time");
        let covering = PreviewDecodeKey::new(
            source.clone(),
            SourceSampleTarget::covering(source_time),
            representation,
            source_color(),
        )
        .expect("covering key");
        let strict = PreviewDecodeKey::new(
            source.clone(),
            SourceSampleTarget::strict_predecessor(source_time),
            representation,
            source_color(),
        )
        .expect("strict predecessor key");

        assert_ne!(covering, strict);
        assert!(matches!(
            PreviewDecodeKey::new(
                source,
                SourceSampleTarget::strict_predecessor(TimelineTime::ZERO),
                representation,
                source_color(),
            ),
            Err(PreviewDecodeContractError::SourceSampleBeforeOrigin { .. })
        ));
    }

    #[test]
    fn data_texture_decode_key_accepts_only_cpu_rgb_representations() {
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/data-texture.mov"),
            exact_fingerprint(42),
            &video_stream(0, PixelFormat::Yuv422p10le, true),
        )
        .expect("valid source with compact-YUV evidence");
        let sample = SourceSampleTarget::covering(TimelineTime::ZERO);
        let data = PreviewSourceColorContract::data_texture(
            super::super::DecodedVideoRangeContract::OverrideFull,
        );

        PreviewDecodeKey::new(
            source.clone(),
            sample,
            PreviewDecodeRepresentation::NativeCpu,
            data,
        )
        .expect("CPU-addressable RGB is the exact DataTexture route");
        for representation in [
            PreviewDecodeRepresentation::NativeSurface,
            PreviewDecodeRepresentation::CompactCpuYuv,
            PreviewDecodeRepresentation::Proxy(Resolution { width: 960, height: 540 }),
        ] {
            assert_eq!(
                PreviewDecodeKey::new(source.clone(), sample, representation, data),
                Err(PreviewDecodeContractError::DataTextureRequiresCpuRgb),
                "{representation:?} must not reinterpret technical channels"
            );
        }
    }

    #[test]
    fn camera_raw_author_controls_rotate_decode_identity_and_request_projection() {
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/frame.dng"),
            exact_fingerprint(43),
            &video_stream(0, PixelFormat::BayerRggb16le, true),
        )
        .expect("valid RAW source");
        let sample = SourceSampleTarget::covering(TimelineTime::ZERO);
        let color = PreviewSourceColorContract::automatic(
            ColorSpace::LinearRec709,
            super::super::DecodedVideoRange::Full,
        );
        let base_intent =
            CameraRawDecodeIntent::new(CameraRawAdapter::Dng, CameraRawInterpretation::default())
                .expect("base RAW intent");
        let raised_intent = CameraRawDecodeIntent::new(
            CameraRawAdapter::Dng,
            CameraRawInterpretation {
                exposure_millistops: 1_000,
                ..CameraRawInterpretation::default()
            },
        )
        .expect("raised RAW intent");
        let base = PreviewDecodeKey::new_camera_raw(
            source.clone(),
            sample,
            PreviewDecodeRepresentation::NativeCpu,
            color,
            base_intent,
        )
        .expect("base RAW key");
        let raised = PreviewDecodeKey::new_camera_raw(
            source.clone(),
            sample,
            PreviewDecodeRepresentation::NativeCpu,
            color,
            raised_intent,
        )
        .expect("raised RAW key");

        assert_ne!(base, raised);
        assert_eq!(base.camera_raw(), Some(base_intent));
        assert_eq!(
            super::super::PreviewDecodeRequest::from_key(
                &base,
                super::super::PreviewDecodeAccessMode::RandomAccessStillFrame,
            )
            .camera_raw,
            Some(base_intent)
        );
        assert_eq!(
            PreviewDecodeKey::new_camera_raw(
                source,
                sample,
                PreviewDecodeRepresentation::NativeSurface,
                color,
                base_intent,
            ),
            Err(PreviewDecodeContractError::CameraRawRequiresCpuFloat)
        );
    }
}
